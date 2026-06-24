# Nanite software rasterizer + streaming (future, test-gated)

> A separate, **high-risk** future optimization on top of the cluster-LOD work
> (Phase B, now shipped — see PR #143 / the deleted `lod.md`). LOD shipping does
> **not** depend on any of this. The Phase 0 spike below is the go/no-go gate; if
> it fails, HW-raster cluster-LOD is the end-state and that is fine.

> **STATUS — IN PROGRESS on the `nanite-streaming` branch** (started after the LOD
> PR #143). Plan/ordering for this branch, gated behind NEW default-off flags so
> the shipped LOD path never regresses:
> 1. **Phase 0 — SW-raster spike (GO/NO-GO), first.** New standalone wasm crate
>    `packages/frontend/bench-nanite-sw-raster/` mirroring `model-tests` (Trunk +
>    `taskfiles/frontend/...` dev server; build the device via renderer-core
>    `AwsmRendererWebGpuBuilder`). Implement HW baseline + Encoding A (packed
>    `atomicMax`) + Encoding B (emulated-64 CAS), a triangle-size + overdraw knob,
>    `performance.now()` timing over many iters, and a payload-image diff vs HW.
>    Drive it via chrome-devtools (navigate → read browser console). RECORD the
>    verdict + numbers here. If NO-GO, skip Phase 3 and go straight to streaming.
> 2. **Phase 5 — streaming (the multi-million-tri fix), independent of Phase 0.**
>    This is the user's priority (multi-million-tri scaling). Start with the
>    intermediate win: cap/page cluster-page residency so the cluster render mesh
>    `M` no longer uploads its FULL exploded geometry (today's ceiling) — GPU
>    feedback marks the pages the cut needs, CPU streams/evicts against a budget,
>    LOD clamps to resident pages. Land gated, show it renders a denser asset than
>    today, document the residual gap, then iterate toward full residency.
> 3. **Phase 3 — hybrid rasterization, ONLY if Phase 0 is GO.**
> Discipline unchanged from the LOD work: gate behind default-off flags
> (flag-off byte-identical), test before every commit, on-device verify via
> chrome-devtools (renderer tracing → BROWSER console), GPU-readback to confirm
> compute output, no per-frame heap allocs, pure-Rust algorithms (wasm target),
> one logical step per commit, never leave the renderer broken (revert+diagnose).

## Why this is split out

Nanite's signature win is rasterizing **sub-pixel triangles** in a compute
shader instead of the hardware rasterizer (HW raster wastes a full 2×2 quad per
triangle → ~25% efficiency or worse for pixel-sized triangles). The compute
rasterizer does a 64-bit `atomicMin(depth << 32 | payload)` per covered pixel:
one atomic that simultaneously depth-tests and records the winning triangle.

**WebGPU has no 64-bit atomics and no extension for them.** WGSL atomics are
`atomic<u32>` / `atomic<i32>` only. So the entire SW-raster advantage hinges on
emulating the depth+payload atomic well enough — correct enough, fast enough —
to beat HW raster for tiny triangles on our target hardware. This is the single
biggest risk in the whole Nanite story, and it is the *only* part that is
genuinely WebGPU-hostile. Anchoring LOD delivery to it would be a mistake — so
it lives here, behind its own gate, and `lod.md` ships without it.

Other WebGPU constraints relevant here:
- **No mesh/task shaders** → cluster expansion is indirect compute dispatch +
  indirect draw, not a mesh-shader amplification stage.
- **No persistent-thread forward-progress guarantee** → the SW-raster scheduler
  must be a fixed multi-pass DAG, not a persistent-thread work queue, and no
  per-pixel spinlock (deadlock risk).

---

## Phase 0 — SW-rasterizer atomic-emulation spike (GO/NO-GO)

> **VERDICT: NO-GO on this WebGPU target (Apple GPU). Phase 3 is NOT built; the
> shipped HW-raster cluster-LOD (Phase B) is the end-state — exactly the outcome
> this doc says is fine. Work pivots to Phase 5 (streaming) for multi-million-tri.**
>
> Spike ran as a self-contained WebGPU bench via chrome-devtools `evaluate_script`
> (own GPUDevice, no editor interference); code + raw numbers in
> `nanite-sw-raster-bench.js`.
> - **Step 1 — atomic-throughput baseline:** ~5.75 G `atomicMax` ops/sec (16.7M /
>   2.92 ms). Harness works on target.
> - **Step 2 — HW baseline vs Encoding A** (packed-u32 `atomicMax` SW raster, the
>   PERF CEILING probe; 16-bit payload, unusable in production), three runs sweeping
>   triangle pixel-size: A is **at best ~1.5–1.8× faster than HW only at sub-pixel
>   (≤1px) sizes — and that margin sits within the sub-ms measurement noise — while
>   A LOSES at ≥2px** (HW ~0.03 ms vs A growing with triangle area). Chrome
>   wall-clock timing is noisy at these sub-ms scales and timestamp-queries are
>   quantized to 100 µs, so a precise margin isn't recoverable, but the *shape* is
>   stable across all three runs: no blowout win.
> - **Why NO-GO:** the doc's gate is "A/B beat HW by a WORTHWHILE margin at small
>   sizes." Not met. A is the *ceiling*; the production Encoding B (emulated-64
>   depth+payload via a CAS spin) is strictly slower than A, so if the ceiling only
>   ties/marginally-beats HW, the realistic encoding loses. Apple's HW rasterizer
>   is efficient even for tiny triangles, and WebGPU's missing 64-bit atomics cap
>   the SW approach — together they remove the sub-pixel-quad advantage SW raster
>   exists to exploit. Building the emulated-atomic SW rasterizer + resolve pass +
>   hybrid routing is not justified by a ~1.5× noisy maybe-win.
> - **Caveat / re-open condition:** measured on one Apple target with a simple
>   one-thread-per-triangle rasterizer. If a future target shows HW raster as a
>   *proven* sub-pixel bottleneck (the original gate condition), re-run with a
>   heavier controlled workload (≥50 ms) to beat the noise before reconsidering.
> - **Phase 3 below is therefore SKIPPED.** Remaining work on this branch =
>   Phase 5 (streaming), independent of the SW rasterizer.

De-risk before building anything. In isolation, implement and benchmark a
compute software rasterizer that emulates the 64-bit depth+payload atomic.

**Harness:** standalone crate `packages/frontend/bench-nanite-sw-raster/` (no
production render passes), with a **triangle-size knob** (SW raster only wins
below the HW quad-efficiency cliff) and an **overdraw knob** (atomic
contention). For each of {HW baseline, A, B} × {size, overdraw}: dispatch/draw →
readback via `new_copy_and_extract_buffer` → time with `performance.now()` over
many iters → (for A/B) diff the payload image vs HW ground truth.

**Decision gate:**
- **GO** (build Phase 3 below): B is correct enough (negligible payload
  mismatch) and at small triangle sizes A/B beat HW by a worthwhile margin.
- **NO-GO**: A is too slow, or B's error rate is visible, or neither beats HW at
  realistic Nanite triangle sizes → stop; HW-raster cluster-LOD (`lod.md`
  Phase B) is the end-state.

### Depth convention

Renderer is reverse-Z and the HZB is **max-reduced** (closer = larger depth).
That lines up with `atomicMax`: largest packed value wins ⇒ closest fragment
wins, *provided depth sits in the high bits*. No convention change needed.

### The bit-budget problem (why one u32 isn't enough for production)

Pack `packed = (depth << PAYLOAD_BITS) | payload`, resolve with `atomicMax`. The
payload must identify the winning surface:
- **triangle-within-cluster**: clusters ≤128 tris ⇒ **7 bits**.
- **visible-cluster index**: indexes the *per-frame compacted visible-cluster
  list* (not a global cluster id). Even a dense scene caps at, say,
  2^18 = 262 144 ⇒ **18 bits**.

Payload ≈ 25 bits, leaving only **7 bits of depth** in a u32 — useless
(z-fighting). This is exactly why a faithful implementation needs 64 bits:
depth(32) + payload(32). On WebGPU we can't have that in one atomic — hence two
encodings to benchmark, a perf ceiling and a realistic one.

### Encoding A — single-u32 packed `atomicMax` (PERF CEILING probe)

`packed = (depth16 << 16) | payload16`, one `atomic<u32>` per pixel, resolved
with `atomicMax`. 16-bit payload is too small for production, but A **measures
the upper bound on atomic-raster throughput** — the single fastest the approach
can ever go (one atomic, no loop, no second store). **If A is already too slow,
full SW raster is dead on WebGPU** — stop, don't even tune B.

### Encoding B — emulated 64-bit (REALISTIC correctness + perf)

Two parallel per-pixel arrays: `depth: array<atomic<u32>>` (full 32-bit) and
`payload: array<u32>` (full 32-bit, non-atomic). Per covered pixel:

```
loop {
  let cur = atomicLoad(&depth[i]);
  if (my_depth <= cur) { break; }                 // reverse-Z: not closer, lose
  let res = atomicCompareExchangeWeak(&depth[i], cur, my_depth);
  if (res.exchanged) { payload[i] = my_payload; break; }
  // else: someone moved depth; retry with res.old_value
}
```

Full depth precision + full 32-bit payload (production-viable), but the payload
store is **not atomic with the depth CAS** — a closer fragment from another
workgroup can land its depth between our successful CAS and our payload write,
leaving payload mismatched for that pixel for one frame.

Measure for B:
1. **Correctness / error rate** — % of pixels whose payload disagrees with the
   final depth winner vs HW ground truth. Expected tiny (only exact sub-pixel
   contention) but must be quantified; visible cracks are unacceptable.
2. **Perf** — the CAS spin adds contention cost under overdraw. Measure vs A and
   vs HW.

**Forward-progress caveat:** WebGPU gives no cross-workgroup forward-progress
guarantee, so a *per-pixel spinlock* (hold lock, write both words, release) is
unsafe — it can deadlock. B deliberately avoids a lock: the CAS loop only spins
on its *own* retry and always terminates (depth is monotonic), accepting the
rare payload race instead of locking. A lock-based race-free variant is out of
scope for the spike.

### HW-raster baseline

Same triangle soup through a minimal pipeline writing `(depth, payload)` to
attachments (mirrors today's geometry pass). Gives the throughput +
ground-truth payload image both encodings are compared against.

### API anchors (renderer-core, verified)

- Device: `AwsmRendererWebGpuBuilder::new(gpu, canvas).with_device_request_limits(DeviceRequestLimits::max_all()).build()` → `AwsmRendererWebGpu { device, .. }`.
- Compute: `compile_shader`, `create_compute_pipeline` (async), `create_bind_group_layout`, `create_bind_group`, `create_command_encoder` → `begin_compute_pass` → `dispatch_workgroups` → `submit_commands`.
- Buffers: `create_buffer`, `write_buffer`, `new_copy_and_extract_buffer` for readback.
- All in `packages/crates/renderer-core/src/{methods.rs,renderer.rs,buffers.rs,command/compute_pass.rs}`.

---

## Phase 3 — Hybrid rasterization (only if Phase 0 is GO)

Route clusters/triangles by screen size:
- **HW-raster path** (large triangles): the `lod.md` Phase B path — compute
  builds a compacted index stream → single `drawIndexedIndirect` → existing
  geometry fragment shader writes the vis buffer.
- **SW-raster path** (sub-pixel triangles): compute rasterizer using the Phase 0
  emulated atomic, writing `(depth|payload)` to a storage target, then a resolve
  pass merges it into the **same** visibility-buffer textures the material
  passes already read.

Both paths converge on one visibility buffer so `material_prep` /
`material_opaque` keep working unchanged.

---

## Phase 5 — Streaming (virtual geometry)

Page-based cluster residency, analogous to virtual texturing. Independent of the
SW rasterizer; kept here because it is the other large, deferrable Nanite bet
that LOD shipping does not require.

> **Step 1 SHIPPED + verified on-device (nanite-streaming): static capped
> residency.** Flag `cluster_streaming` (default-off, `?stream`). The loader
> (`select_resident_clusters`, scene-loader) caps the cluster render mesh `M` to a
> triangle budget — `M`'s exploded buffer (56 B/index) is the multi-million-tri
> ceiling; cluster metadata is tiny. It keeps the coarsest clusters up to the
> budget (hard cap), clamps each resident **leaf** `lod_error→0` for watertight
> close-up, and remaps `first_index` into the compacted `M`. **No shader change** —
> the per-cluster GPU cut/compaction/draw just see fewer pages + a smaller `M`.
> - On-device (DamagedHelmet, budget temporarily lowered to 8 000 to force the
>   cap): `13616 clusters (1006 resident), M = 8000 tris (CAPPED from 43140)`; the
>   GPU cut then drew `610 tris over 1006 clusters` and the helmet rendered
>   **watertight with full materials** at 60 fps. Budget passthrough (helmet under
>   the default 1.0M budget) logs `13616 resident` and is byte-identical to `?vg`.
>   Flag off ⇒ verbatim passthrough. 3 unit tests pin the cap/remap/leaf-clamp.
 - **Dense-asset scaling check:** a subdivided-sphere asset (1 024-tri sphere +
>   Subdivide×4 = 262 144 tris, baked to a **550 856-tri cluster DAG / 17 951
>   clusters**) loaded under `?vg&streambudget=8000`: capped to **7 970 tris (776
>   resident clusters)**, the GPU cut ran (drew 1 060 tris) at 60 fps — confirming
>   the cap/load path scales to a large mesh (69× reduction) without overflow.
> - **Caveat found (NOT a streaming regression):** that subdivided sphere renders
>   with **cluster-cut holes at FULL detail too** (no cap), i.e. the holes are a
>   **pre-existing Phase B cut/bake issue on this `watertight:false` synthetic mesh**
>   (midpoint-subdivision topology), independent of capping — the cap just selects
>   fewer clusters, it does not introduce the holes. Real glTF assets (the
>   DamagedHelmet) cluster + cut + cap **watertight**; the synthetic non-watertight
>   sphere does not. Follow-up (Phase B / a separate issue, out of this branch's
>   scope): make the cluster bake/cut robust to non-watertight + subdivided input.
> - **Residual gap (→ Step 2):** the cap is *static* (chosen at load); `M`'s detail
>   is bounded by the budget regardless of camera, and seams can appear where the
>   partial frontier level borders coarser-only regions. Positions aren't capped
>   (smaller buffer than the exploded geometry). True per-frame paging below closes
>   these.

### Step 2 (design note) — dynamic paging

Step 1 caps detail *statically* at load. Step 2 makes residency **per-frame and
camera-driven**: the cut asks for finer pages where the camera is close, the CPU
streams them in and evicts cold ones, so a multi-million-tri asset shows full
detail near the camera within a bounded VRAM budget. This is a design note, not
yet built — the GPU feedback + async paging + eviction is a multi-day effort and
the standing rule is "ship Step 1 + design over half-built code."

**What changes vs Step 1.** Today `M` is ONE contiguous exploded buffer and the
compaction emits identity indices into it (the `first_index` remap). Paging breaks
the "one contiguous M" assumption: geometry must live in **fixed-size page slots**
so individual clusters can be uploaded/evicted independently.

**GPU residency pool (replaces the monolithic M).**
- `page_pool`: a fixed-capacity buffer of `P` slots, each holding one cluster's
  exploded geometry at the bake's max cluster size (≤128 tris ⇒ ≤384 exploded
  verts × 56 B ≈ 21 KB/slot; e.g. P = 8 192 slots ≈ 168 MB — the VRAM budget knob).
- `resident: array<i32>` length = `cluster_count`: cluster_id → pool slot, or `-1`
  if not resident. The single source of truth the cut reads.
- `slot_meta: array<{cluster_id, last_used_frame}>` length `P`: reverse map for
  eviction (LRU).
- CPU keeps the baked `ClusterMesh` page geometries host-side (or mmaps the bundle)
  as the stream source; "disk/network streaming" is a later refinement of the
  *source*, orthogonal to the GPU paging mechanics here.

**Per-frame data flow (reuses the existing cut → compaction → draw):**
1. **Cut (extended `cluster_cut.wgsl`).** For each cluster the cut would select,
   check `resident[id]`. If resident → emit as today (slot index instead of
   `first_index`). If NOT resident → (a) walk up to the nearest resident ancestor
   and emit THAT (crack-free coarse fallback — exactly Step 1's clamp, but the
   "frontier" is now wherever residency currently reaches), and (b) `atomicOr`/
   append `id` into a **`feedback` buffer** (a bitset or a compacted append list)
   marking "wanted but absent". Also bump `slot_meta[slot].last_used_frame` for
   every resident slot it used (LRU touch).
2. **Compaction (unchanged shape).** Packs the selected (resident) slots' indices
   into the compacted stream + draw args, but indices are now `slot*PAGE_VERTS + k`
   into `page_pool` rather than into a contiguous M.
3. **Readback (async, amortized).** Copy `feedback` → MAP_READ (the existing
   readback pattern), one frame latent. CPU gets the set of wanted-absent
   cluster_ids. No per-frame stall: the draw used the coarse fallback this frame;
   the finer page appears a frame or two later (progressive refinement, like
   virtual texturing).
4. **Stream + evict (CPU).** For each wanted id: find a free slot, or evict the
   slot with the oldest `last_used_frame` (skip slots used this frame). Upload that
   cluster's exploded geometry (`writeBuffer` into the slot) and set
   `resident[id] = slot`. Cap uploads/frame to a byte budget so a big jump doesn't
   hitch. Clear evicted ids to `-1` first so the cut can't read a half-evicted slot.

**Crack-free.** The cut still only ever emits a valid DAG antichain (the same
group-consistent `lod_bounds` tiling as Step 1); the coarse-fallback when a page is
absent is itself a valid (coarser) antichain, so transitions stay watertight — they
just refine over a few frames as pages arrive. The resident-leaf `lod_error→0`
clamp from Step 1 becomes dynamic (applied to whichever clusters are currently the
finest resident on each path).

**Why it stays within budget.** Working-set = sum of the visible LOD cuts' pages,
not the whole asset; cold pages (off-screen / far) evict. The pool size is the hard
VRAM cap; detail degrades gracefully (coarser) when the budget is saturated rather
than overflowing — the property Step 1 already gives, now camera-adaptive.

**Build order when picked up:** (1) page_pool + resident table + port the cut to
read `resident` and emit slot indices (no feedback yet — static residency through
the pool, proving the indirection); (2) add the feedback buffer + readback +
CPU upload-into-slot (no eviction — grow-only until full); (3) LRU eviction +
per-frame upload budget; (4) multi-million-tri on-device verification + `?stress`.
Each gated behind `cluster_streaming` (or a new `cluster_paging`) flag, default-off.

**Ties into** `PERFORMANCE_OPEN_WORLD_PLAN.md` (the VRAM-budget / LRU machinery is
shared with texture streaming). The cost that scales with "distinct meshes" is
memory/streaming (N unique datasets resident vs one when instanced); this paging is
what bounds the working set to the sum of visible LOD cuts.

## Verification

- Bake a multi-million-triangle reference asset; compare SW-raster vs HW-raster
  for visual parity (payload-image diff + screenshots via chrome-devtools MCP).
- Cross-check the vis buffer via the existing GPU picker compute path.
- Stress with `?stress=N` / `?trace=sub-frame`; no per-frame heap allocs in the
  hot path.
