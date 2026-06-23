# Nanite-style virtualized geometry for AwsmRenderer

> Roadmap / SSOT. Phase 0's detailed encoding design is in this doc's
> [Phase 0 appendix](#phase-0-appendix--sw-rasterizer-bit-budget-detail).

## Context

**Goal:** support scenes with millions of triangles via Nanite-style virtualized geometry — cluster decomposition, GPU-driven per-cluster culling, continuous (seamless) LOD, a compute **software rasterizer** for sub-pixel triangles, and disk streaming. Scope:

- **Full Nanite, including the software rasterizer + streaming** (not just cluster-LOD).
- **Static rigid meshes only** — "static" = the geometry doesn't *deform* (no skin/morph), **not** that the object can't move. Per-object translate/rotate/scale is fully in scope; thousands of independently moving rigid meshes are the normal case. Skin/morph is excluded because per-vertex deformation invalidates the baked cluster bounds/error/topology.
- **Offline asset-bake tool** builds clusters + the LOD DAG (not at load time).

**Why this renderer is a good fit (the head start).** It is already a *GPU-driven visibility-buffer deferred renderer*, which is exactly Nanite's backbone. We reuse, not rebuild:

- **Visibility buffer** — `render_passes/geometry/` already writes per-pixel `triangle_index + material_mesh_meta_offset` (packed into `visibility_data` RGBA16uint), plus barycentrics, normal/tangent, and bary derivatives. See `geometry/shader/geometry_wgsl/fragment.wgsl`.
- **GPU culling** — frustum + Hi-Z occlusion in compute (`render_passes/occlusion/`, `render_passes/hzb/`), feeding a compaction compute pass that writes `drawIndexedIndirect` args (`render_passes/occlusion/compaction.rs`).
- **Deferred material resolve** — `material_prep/` + `material_opaque/` compute passes reconstruct attributes from the vis buffer and shade. Adding clusters mostly changes *where vertex indices come from*, not the shading model.
- **Compute ecosystem, async pipeline cache, BVH spatial index** — all present and mature.

**Why this beats today's path for many *distinct* meshes (the motivating case).** The current pipeline issues **one `drawIndexedIndirect` slot per `MeshKey`** (`compaction.rs`), and WebGPU has no `multiDrawIndirect` — so 5000 separate (non-instanced) meshes = 5000 draw calls and CPU submission cost that scales with object count. Nanite decouples cost from object/mesh count entirely:

- All meshes bake into a **single global cluster space**; a scene object is just an instance record `(transform, cluster-hierarchy-root, material)`. 5000 distinct meshes = 5000 roots; 5000 instances of one mesh = one shared root. The cull/raster pipeline treats both identically.
- **Two-level cull**: cheap per-instance frustum/HZB test over the 5000 bounds (generalizes the existing `OcclusionInstance` array, which already handles non-instanced meshes), then per-cluster LOD only inside survivors — never touching every cluster of every mesh.
- **Per-instance LOD cut bounds the triangle count**: distant copies collapse to coarse clusters, so millions of *source* triangles reduce to a bounded *visible-cluster* budget regardless of raw totals.
- **One draw + one dispatch, not 5000**: compaction merges visible clusters from all meshes into one compacted stream → a single `drawIndexedIndirect` (HW) + single `dispatchWorkgroupsIndirect` (SW). Object count drops out of submission cost.
- The cost that *does* scale with "distinct": **memory/streaming** — 5000 unique datasets resident vs one when instanced. That is exactly what Phase 5 (paged streaming + VRAM budget) handles; the working set is the sum of the visible LOD cuts.

**The hard truth up front (WebGPU constraints).** The signature Nanite win — rasterizing sub-pixel triangles in compute via a 64-bit `atomicMin(depth<<32 | payload)` — is **not directly expressible in WGSL**. WebGPU has:
- **No `atomic<u64>`** and no 64-bit-atomic extension → the SW rasterizer's depth+payload write must be emulated (see Phase 0). This is the single biggest risk and erodes the perf advantage that justifies a SW rasterizer at all.
- **No mesh/task shaders** → cluster expansion is done with indirect compute dispatch + indirect draw, not a mesh-shader amplification stage.
- **No `multiDrawIndirect`** → can't cheaply issue N per-cluster draws. The existing indirect path is strictly per-`MeshKey` (one args slot each). The HW-raster cluster path must instead build *one* compacted geometry stream via compute and issue a single `drawIndexedIndirect`.
- **No persistent-thread forward-progress guarantee** → the LOD/cull scheduler must be a fixed multi-pass DAG traversal, not a persistent-thread work queue.
- **Tight binding limits** (~10 storage buffers/stage; baseline 8) — cluster metadata bindings must be budgeted.

**Honest sizing.** This is a multi-month, research-grade effort. **Phase 0 is a go/no-go spike** on the SW rasterizer; if it can't hit acceptable perf, the pragmatic fallback (Phases 1–4 with HW raster only) still delivers ~80% of the benefit at a fraction of the risk. Everything ships behind a default-off feature flag (mirroring `gpu_culling`/`decals` in `features.rs`) so existing consumers are unaffected, and lands **after the in-flight §D uber-shader work**.

---

## Phase 0 — SW-rasterizer atomic-emulation spike (GO/NO-GO)

De-risk the whole project before building infrastructure. In isolation, implement and benchmark a compute software rasterizer that emulates the 64-bit depth+payload atomic on WebGPU. **Full design + bit-budget analysis: [`nanite-phase0-sw-raster-spike.md`](nanite-phase0-sw-raster-spike.md).** Candidate encodings:

- **Encoding A — single-u32 packed `atomicMax`** (`(depth16 << 16) | payload16`): a *perf-ceiling probe*. Payload too small for production, but measures the fastest the atomic path can ever go. If A is too slow, full Nanite SW raster is dead on WebGPU — stop here.
- **Encoding B — emulated 64-bit** via an `atomicCompareExchangeWeak` spin on a full 32-bit depth word + a non-atomic 32-bit payload store: production-viable bit budget, but the payload store races the depth CAS. Measure both **error rate** vs HW ground truth and **perf** under overdraw. No per-pixel spinlock (WebGPU has no forward-progress guarantee → deadlock risk).

Harness: standalone crate `packages/frontend/bench-nanite-sw-raster/` (no production render passes), with a **triangle-size knob** (SW raster only wins below the HW quad-efficiency cliff) and an **overdraw knob** (atomic contention). For {HW baseline, A, B}: dispatch/draw → readback via `new_copy_and_extract_buffer` → time with `performance.now()` over many iters → diff payload image vs HW ground truth.

**Decision gate:** B correct enough + A/B beat HW at small triangle sizes → full plan. Else → drop SW raster, ship HW-raster cluster-LOD (Phases 1–4 only).

---

## Phase 1 — Offline bake tool + cluster data model

**What "bake" means (to avoid confusion):** the bake operates on a **single mesh asset's geometry**, once, at content-build / asset-import time — like generating texture mipmaps or LOD meshes. It is **decoupled from the scene**: it knows nothing about placement, instance count, or motion. `car.gltf` → `car.nanite` (clusters + LOD DAG). At runtime, *N* copies of that car (10,000 driving cars = 10,000 instance records → transform + material) all reference the **one** baked dataset; distinct car models are each baked once and coexist in the pool. Spawning/moving a copy requires **zero** runtime baking — only cull + LOD-cut + raster over what's visible. Think "automated continuous LODs authored offline," with the per-cluster cut chosen at runtime per camera.

**New crate** (e.g. `awsm-nanite-bake`, sibling to `awsm-meshgen`):
- Cluster generation (~128 tris/cluster) via `meshopt` crate (`meshopt_buildMeshlets`).
- **LOD DAG** (the seamless-LOD core): group adjacent clusters (graph partition, e.g. `metis`), simplify each group with **locked shared boundaries** (`meshopt_simplify` boundary-lock), re-split into coarser clusters. Record per-group **monotonic error** + bounding sphere. Boundary locking is what makes LOD transitions crack-free — non-negotiable.
- Emit a new asset format / glTF extension: cluster vertex pages, cluster index pages, per-cluster meta (local bounds, group parent/child links, LOD error, material id).

**Ingestion:** extend `mesh_pack.rs` / `raw_mesh.rs` / the scene-loader to load cluster pages into GPU buffers (cluster vertex pool, index pool, per-cluster meta storage buffer). The existing 56-byte exploded visibility vertex layout is retained per-cluster.

**Transforms:** clusters are baked in **object space** and are transform-invariant — a moving instance reuses the same pages/DAG. The per-instance rigid transform stays in today's `TransformKey` / transform buffer (`transforms.rs`, `instances.rs`), updated per frame as now; the cull/LOD/raster passes consume the live transform each frame (no precomputation pins geometry to a world position).

---

## Phase 2 — Cluster culling + LOD selection (compute)

Extend the existing occlusion/compaction machinery from per-mesh to per-cluster:
- **Two-pass occlusion** (Nanite-style, reusing `render_passes/hzb/`): pass 1 rasterizes last-frame-visible clusters and builds the HZB; pass 2 tests the remainder against it.
- **LOD cut selection**: per cluster *group*, compare projected screen-space error vs threshold to choose the DAG cut (parent vs children). Output the visible cluster list. The projection uses the **instance world transform incl. scale** (a scaled-up instance projects larger → descends to finer clusters automatically). Cull also transforms the cluster's object-space bounding sphere to world space per instance; **non-uniform scale/skew** needs conservative bounds (AABB/OBB) + error scaled by the max axis.
- **Compaction**: extend `render_passes/occlusion/compaction.rs` to emit per-cluster draw/dispatch args. Since there is no `multiDrawIndirect`, compaction builds **one compacted stream** (visible cluster list + a packed index buffer for the HW path) consumed by a single indirect draw, plus `dispatchWorkgroupsIndirect` args for the SW path.

---

## Phase 3 — Hybrid rasterization

Route clusters/triangles by screen size:
- **HW-raster path** (large triangles): compute builds a compacted index stream over visible clusters → single `drawIndexedIndirect` → the existing geometry fragment shader (`geometry/shader/geometry_wgsl/fragment.wgsl`) writes the vis buffer. Add a **cluster id** to the written payload (see Phase 4).
- **SW-raster path** (sub-pixel triangles): compute rasterizer using the Phase 0 emulated atomic, writing `(depth|payload)` to a storage target, then a resolve pass merges it into the same visibility-buffer textures the material passes already read.

Both paths must converge on **one** visibility buffer so `material_prep` / `material_opaque` keep working.

---

## Phase 4 — Visibility buffer + material integration

- **Vis-buffer payload**: today `visibility_data` = `triangle_index` + `material_mesh_meta_offset` (each a u32 split into 2×u16). Re-budget it to carry `cluster_id` + `triangle-in-cluster` + material routing. Update `pack_normal_tangent`/`split16`/`join32` usage in `fragment.wgsl` and the readers.
- **Attribute reconstruction**: `material_prep/shader/material_prep_wgsl/compute.wgsl` and `material_opaque/.../compute.wgsl` currently fetch triangle vertex indices from the per-mesh geometry pool. Re-point them at **cluster index pages** (`cluster_id` → page → 3 vertex indices → barycentric interpolation). Material id now resolves via cluster meta.
- **Respect the prep-vs-recompute standard** (`docs/SHADER_GUIDELINES.md`) and the **MSAA-compile invariant** — the edge-resolve compute path must stay consistent; edges are now cluster-scale, flag as a standards-review item.

---

## Phase 5 — Streaming (virtual geometry)

Page-based cluster residency, analogous to virtual texturing:
- GPU feedback buffer marks the cluster pages the LOD cut needs this frame.
- CPU streams requested pages from disk/network into the GPU pools and evicts cold pages (LRU against a VRAM budget — ties into `PERFORMANCE_OPEN_WORLD_PLAN.md`).
- LOD selection clamps to resident pages until higher-detail pages arrive.

---

## Cross-cutting

- **Feature gate**: add `virtual_geometry` (or `nanite`) to `features.rs`, **default off**, mirroring `gpu_culling`. Zero-risk to existing consumers; the whole pipeline only activates when on.
- **Sequencing**: land after the uber-shader / dispatch-grouping work to avoid colliding with it.
- **Skinned/morph meshes**: explicitly excluded — they keep today's per-mesh visibility path. Cluster and non-cluster geometry coexist in the same vis buffer. (Rigid per-object transforms are *not* excluded — see scope note and Phase 2.)

## Critical files

- Vis-buffer write: `packages/crates/renderer/src/render_passes/geometry/shader/geometry_wgsl/{vertex,fragment}.wgsl`, `geometry/pipeline.rs`
- Culling/LOD/indirect: `render_passes/occlusion/{cull.wgsl,compaction.rs,buffers.rs}`, `render_passes/hzb/`
- Material resolve: `render_passes/material_prep/shader/.../compute.wgsl`, `render_passes/material_opaque/shader/.../compute.wgsl`
- Geometry packing/ingestion: `src/mesh_pack.rs`, `src/raw_mesh.rs`, scene-loader; new `awsm-nanite-bake` crate
- Scheduling/features: `src/render.rs`, `src/features.rs`

## Verification

- Bake a multi-million-triangle reference asset; load it with `task model-tests:dev` (port 9080) and the editor (`task editor-dev`, port 9085).
- Use **chrome-devtools MCP** for perf traces (frame time, triangle throughput) and screenshots; compare SW-raster vs HW-raster reference for visual parity.
- Correctness: cross-check the vis buffer via the existing GPU **picker** compute path; confirm crack-free LOD transitions while dollying the camera (boundary-lock validation).
- Stress: `?stress=N` and `?trace=sub-frame`; confirm no per-frame heap allocs in the hot path (David's standard).
- Gate hygiene: with the feature **off**, assert byte-identical output to today (default-must-equal-today).
- Per-phase: `cargo test -p awsm-renderer -p awsm-materials -p awsm-scene-loader --lib` before each commit.

---

## Phase 0 appendix — SW-rasterizer bit-budget detail

Detailed design for the Phase 0 go/no-go spike. This is the analysis that
shapes what the benchmark must measure and *why*.

### What we're deciding

Nanite's signature win is rasterizing **sub-pixel triangles** in a compute
shader instead of the hardware rasterizer — hardware raster wastes a full 2×2
quad per triangle, so pixel-sized triangles run the fixed-function unit at
~25% efficiency or worse. The compute rasterizer does a 64-bit
`atomicMin(depth << 32 | payload)` per covered pixel: one atomic op that
*simultaneously* depth-tests and records which triangle won.

**WebGPU has no 64-bit atomics and no extension for them.** WGSL atomics are
`atomic<u32>` / `atomic<i32>` only. So the whole question is: can we emulate
the depth+payload atomic well enough — correct enough, fast enough — that a
compute SW rasterizer beats HW raster for tiny triangles on our target
hardware? If no, we drop SW raster and ship HW-raster cluster-LOD
(Phases 1–4), which still delivers most of the benefit.

### Depth convention

This renderer is reverse-Z and the HZB is **max-reduced** (closer = larger
depth value). That lines up with `atomicMax`: the largest packed value wins ⇒
the closest fragment wins, *provided depth sits in the high bits*. No
convention change needed.

### The bit-budget problem (why one u32 isn't enough for production)

Pack one u32 per pixel as `packed = (depth << PAYLOAD_BITS) | payload`,
resolved with `atomicMax`. The payload must identify the winning surface:

- **triangle-within-cluster**: clusters are ≤128 tris ⇒ **7 bits**.
- **visible-cluster index**: this indexes the *per-frame compacted
  visible-cluster list*, NOT a global cluster id. Even a very dense scene
  caps visible clusters at, say, 2^18 = 262 144 ⇒ **18 bits**.

Payload ≈ 18 + 7 = **25 bits**, leaving only **7 bits of depth** in a u32 —
useless (massive z-fighting). This is exactly why a faithful implementation
needs 64 bits: depth(32) + payload(32). On WebGPU we can't have that in one
atomic. Hence two encodings to benchmark — a perf ceiling and a realistic one.

### Encoding A — single-u32 packed `atomicMax` (PERF CEILING probe)

`packed = (depth16 << 16) | payload16`, one `atomic<u32>` per pixel, resolved
with `atomicMax`. 16-bit payload is too small for production, but that's fine
— **A measures the upper bound on atomic-raster throughput** on this GPU. It's
the single fastest the approach can ever go (one atomic, no loop, no second
store). Decision logic: **if A is already too slow, full Nanite SW raster is
dead on WebGPU** and we stop — no need to even tune B.

### Encoding B — emulated 64-bit (REALISTIC correctness + perf)

Two parallel per-pixel arrays: `depth: array<atomic<u32>>` (full 32-bit depth)
and `payload: array<u32>` (full 32-bit payload, non-atomic). Per covered pixel:

```
loop {
  let cur = atomicLoad(&depth[i]);
  if (my_depth <= cur) { break; }                 // reverse-Z: not closer, lose
  let res = atomicCompareExchangeWeak(&depth[i], cur, my_depth);
  if (res.exchanged) { payload[i] = my_payload; break; }
  // else: someone moved depth; retry with res.old_value
}
```

Full depth precision and a full 32-bit payload (production-viable), but the
payload store is **not atomic with the depth CAS** — a closer fragment from
another workgroup can land its depth between our successful CAS and our payload
write, leaving payload mismatched for that pixel for one frame.

Measure for B:
1. **Correctness / error rate** — % of pixels whose payload disagrees with the
   final depth winner, vs HW-raster ground truth. Expected tiny (only exact
   sub-pixel contention) but must be quantified; visible cracks are
   unacceptable.
2. **Perf** — the CAS spin adds contention cost under overdraw. Measure vs A
   and vs HW baseline.

**Forward-progress caveat:** WebGPU gives no cross-workgroup forward-progress
guarantee, so a *per-pixel spinlock* (hold lock, write both words, release) is
unsafe — it can deadlock. B deliberately avoids a lock: the CAS loop only
spins on its *own* retry and always terminates (depth is monotonic), accepting
the rare payload race instead of locking. A lock-based race-free variant is
explicitly out of scope for the spike.

### HW-raster baseline

Same triangle soup drawn through a minimal render pipeline writing
`(depth, payload)` to attachments (mirrors today's geometry pass). Gives the
throughput + ground-truth payload image both encodings are compared against.

### Benchmark harness

Standalone crate `packages/frontend/bench-nanite-sw-raster/`, no production
render passes. Generates a parametric triangle soup with a **triangle-size
knob** (SW raster only wins below the quad-efficiency cliff) and an **overdraw
knob** (atomic contention). For each of {HW, A, B} × {size, overdraw}:
dispatch/draw, read back via `new_copy_and_extract_buffer`, time with
`performance.now()` over many iterations, and (for A/B) diff the payload image
against HW ground truth.

Outputs feed the **GO/NO-GO**:
- GO (full Nanite): B is correct enough (negligible mismatch) and at small
  triangle sizes A/B beat HW by a worthwhile margin.
- NO-GO (HW-raster cluster-LOD only): A is too slow, or B's error rate is
  visible, or neither beats HW at realistic Nanite triangle sizes.

### API anchors (renderer-core, verified)

- Device: `AwsmRendererWebGpuBuilder::new(gpu, canvas).with_device_request_limits(DeviceRequestLimits::max_all()).build()` → `AwsmRendererWebGpu { device, .. }`.
- Compute: `compile_shader`, `create_compute_pipeline` (async), `create_bind_group_layout`, `create_bind_group`, `create_command_encoder` → `begin_compute_pass` → `dispatch_workgroups` → `submit_commands`.
- Buffers: `create_buffer` (`BufferDescriptor`/`BufferUsage`), `write_buffer`, `new_copy_and_extract_buffer` for readback.
- All in `packages/crates/renderer-core/src/{methods.rs,renderer.rs,buffers.rs,command/compute_pass.rs}`.
