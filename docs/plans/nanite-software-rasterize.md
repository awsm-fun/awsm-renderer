# Cluster-LOD streaming + bake robustness — remaining-work plan

> **What this doc is.** The forward plan for finishing the "virtual geometry"
> (Nanite-style cluster LOD) story after the two PRs below land. It is written to
> be picked up cold in a fresh session. The historical software-rasterizer spike
> lives at the bottom as a **settled NO-GO** (don't re-litigate it) — the filename
> is kept for continuity, but the *remaining* work is streaming + bake robustness,
> not a software rasterizer.

> **Read first, in a fresh session:** this doc, then the memory note
> `lod-nanite-overnight-outcome`, then skim the two PRs (#143, #144). The on-device
> mechanics (editor :9085, `?vg`/`?stream` flags, trunk rebuild, MCP pairing) are in
> the memory notes `mcp-improvements-loop-mechanics` +
> `renderer-tracing-in-browser-console`.

---

## Status — what's shipped vs what remains

**Shipped (gated default-off ⇒ flag-off is byte-identical; no non-LOD regression):**

- **PR #143 → `main` — built-in LOD.** Phase A discrete LOD (static/skinned/morph:
  per-mesh toggle + `set_mesh_lod` MCP + inspector UI, pure-Rust boundary-locked QEM
  bake in `awsm-renderer-lod-bake`, per-instance screen-error selection). Phase B
  cluster GPU virtual geometry (cluster DAG bake → per-cluster GPU cut
  `cluster_cut.wgsl` → compaction `cluster_compaction.wgsl` → `drawIndexedIndirect`
  into the shared visibility buffer). Flags: `lod`, `virtual_geometry` (`?vg`).
- **PR #144 (draft) → `lod-nanite` (stacked) — streaming Step 1.** Capped
  cluster-page residency: flag `cluster_streaming` (`?stream`) + `?streambudget=N`
  (`RendererFeatures.cluster_streaming_budget: Option<usize>`).
  `select_resident_clusters` (scene-loader) caps the cluster render mesh `M` to a
  triangle budget, **CPU-side, no shader change**. Plus the Phase 0 SW-raster
  **NO-GO** record (appendix).

**Remaining gaps (this plan):**

- **Gap A — bake/cut robustness on non-watertight / subdivided meshes** (a real bug
  in the *shipped* Phase B; affects #143). The highest-value scoped fix.
- **Gap B — streaming Step 2: dynamic per-frame paging** (the full multi-million-tri
  solution; Step 1 is the static intermediate). The big feature.
- **Gap C — minor polish** (uncapped positions; aggressive-cap frontier seams) —
  mostly subsumed by Gap B.

---

## How we get multi-million-tri **without** the software rasterizer

The Phase 0 NO-GO does **not** block multi-million-tri. They are different problems:

- The **software rasterizer** is a *rasterization-speed* optimization for the
  extreme "≈1 triangle per pixel" regime, where the HW rasterizer wastes a 2×2 quad
  per sub-pixel triangle. The spike found HW raster is efficient enough on our
  target that emulating a 64-bit atomic in a compute rasterizer doesn't pay off. So
  we keep the **hardware rasterizer**. That's the whole cost of the NO-GO: nothing.

- **Multi-million-tri scaling is a *memory + draw-count* problem**, solved by two
  independent bounds, both on the HW rasterizer:
  1. **Bounded draw — the LOD cut (shipped, Phase B).** The per-cluster cut only
     ever emits the clusters visible *at this camera distance*, sized to a
     screen-space-error budget. A 50M-tri source still only draws the cut
     (~hundreds-of-k to a couple-million triangles), and the cut deliberately keeps
     triangles **near pixel-sized, not sub-pixel** — which is exactly the regime
     where HW raster is fine and the SW rasterizer would have been irrelevant.
  2. **Bounded storage — streaming residency (Step 1 shipped, Step 2 = Gap B).**
     `M`'s exploded vertex buffer (56 B/index of the *whole* DAG) is the VRAM
     ceiling. Step 1 caps it statically at load; Step 2 makes residency
     camera-driven (page in near, evict far) so VRAM holds only the working set.

So: **LOD cut (bounded draw) + streaming residency (bounded VRAM), on HW raster** =
multi-million-tri. The SW rasterizer was only ever a *further* speed refinement for
a density we don't need to hit, which is why it's correctly parked.

**What bounds the cut (why ~hundreds-of-k–2M triangles is renderable, not a wall).**
The cut's triangle count is tied to **screen resolution + the pixel-error budget,
NOT the source size** — the same draw whether the source is 1M or 500M tris (1080p ≈
2.1M px, 4K ≈ 8.3M px; at a few px/triangle the cut is ~a few hundred thousand to
~2M tris). That's cheap here because the renderer is **visibility-buffer deferred**:
- the **geometry pass only writes triangle-ID + barycentrics** — a thin raster pass
  (modern GPUs set up triangles at billions/sec ⇒ ~2M tris is sub-ms to ~2 ms, well
  inside a 16.6 ms frame);
- **material shading is deferred + per-pixel**, so its cost scales with screen
  pixels, not triangle count (adding source detail adds no shading work);
- the **two-level HZB occlusion cull** drops occluded clusters *before* the draw, so
  you rasterize only the visible part of the cut.

The one place this strains is pushing toward literal **1 triangle per pixel**, where
the fixed-function rasterizer's 2×2-quad granularity wastes up to ~4× the
geometry-pass fragment work — the exact regime the parked SW rasterizer targets. The
pixel-error budget is the knob to stay above that cliff (and shrink the cut). Real
numbers come from the benchmarking step in Acceptance, once Gap A + Gap B land.

---

## Gap A — cluster bake/cut robustness on non-watertight / subdivided meshes

**Symptom (reproduced).** A `meshgen` sphere + `Subdivide×4` (262k tris) baked to a
550 856-tri cluster DAG renders with **cluster-cut holes** — missing triangles —
**at full detail** (`?vg`, no cap). Real glTF assets (DamagedHelmet) cluster + cut
**watertight**. The streaming cap does *not* cause this (holes are present uncapped);
it's a pre-existing Phase B defect, surfaced by `watertight:false` / midpoint-
subdivision topology.

**Why it matters.** It's a robustness gap in the feature #143 actually ships.
Procedural / sculpted / non-manifold meshes are common; silently dropping triangles
is a correctness bug.

**Repro (fresh session):**
1. Bring up the editor (`?vg`) — see the on-device mechanics in the memory notes.
2. `insert_primitive sphere` → `add_modifier {"subdivide":{"iterations":4}}` →
   `load_player_bundle` → orbit close (`set_camera_orbit radius 2`) → `screenshot_scene`.
   You'll see fractured/holey coverage. The same asset without subdivision (or a
   real glTF) is watertight.

**Suspected causes (investigate in order):**
1. **Bake boundary classification.** The QEM/cluster bake classifies edges as
   Interior/Boundary/Corner. On a `watertight:false` mesh, "boundary" edges are
   everywhere; if the cluster **grouping** or the **DAG simplify** mishandles open
   edges, adjacent clusters' shared edges may not stay locked ⇒ the per-cluster cut
   (which assumes group-consistent locked boundaries) tears. Check
   `awsm-renderer-lod-bake` (`dag.rs`, `cluster_mesh.rs`, `simplify.rs`).
2. **Degenerate / duplicated geometry from `Subdivide`.** Midpoint subdivision drops
   per-vertex attrs and may produce welds the cluster builder doesn't expect. Dump
   the baked `ClusterMesh` for the sphere and check: do the level-0 clusters'
   triangles reconstruct the source mesh exactly (the bake has a
   `base_triangle_count` invariant + a test for this)? If not, the bake is lossy on
   this input.
3. **Cut coverage at full detail.** At close range the cut should select the level-0
   clusters covering the whole surface. Add a GPU-readback of the cut's `selected`
   set (the pattern already exists) and confirm every surface region has a selected
   cluster; if regions are uncovered, the DAG's error-interval **tiling** is broken
   for this mesh (some path's `[lod_error, parent_error)` intervals don't tile
   `[0,∞)`), which points back to (1).

**Fix + verify.** Make the bake robust to open/non-manifold edges (lock all open
boundary edges; ensure the group-consistent bounds + error-interval tiling hold for
every path even with boundaries). Add a unit test in `awsm-renderer-lod-bake` that
bakes a known non-watertight mesh and asserts the DAG tiles + level-0 reconstructs
the source. Verify on-device: the subdivided sphere renders **watertight** at full
detail and under `?streambudget`. Gate any behavior change so real-asset output is
unchanged (this is a bake-correctness fix, not a flag).

---

## Gap B — streaming Step 2: dynamic per-frame paging (full multi-million-tri)

Step 1 caps detail *statically* at load (`M`'s detail is bounded by the budget
regardless of camera). Step 2 makes residency **per-frame and camera-driven**: the
cut asks for finer pages where the camera is close, the CPU streams them in and
evicts cold ones, so a multi-million-tri asset shows full detail near the camera
within a bounded VRAM budget. Multi-day effort — build it incrementally, each step
gated + tested + on-device-verified.

**What changes vs Step 1.** Today `M` is ONE contiguous exploded buffer and the
compaction emits identity indices into it (the `first_index` remap in
`select_resident_clusters`). Paging breaks the "one contiguous `M`" assumption:
geometry must live in **fixed-size page slots** so clusters upload/evict
independently.

**GPU residency pool (replaces the monolithic `M`):**
- `page_pool`: fixed-capacity buffer of `P` slots, each holding one cluster's
  exploded geometry at the bake's max cluster size (≤128 tris ⇒ ≤384 exploded verts
  × 56 B ≈ 21 KB/slot; e.g. `P = 8192` ≈ 168 MB — the VRAM-budget knob).
- `resident: array<i32>` length `cluster_count`: cluster_id → slot, or `-1`. The
  single source of truth the cut reads.
- `slot_meta: array<{cluster_id, last_used_frame}>` length `P`: reverse map for LRU.
- CPU keeps the baked `ClusterMesh` page geometries host-side (or mmaps the bundle)
  as the stream source. Disk/network streaming is a later refinement of the
  *source*, orthogonal to the GPU paging mechanics.

**Per-frame data flow (reuses the existing cut → compaction → draw):**
1. **Cut (extend `cluster_cut.wgsl`).** For each cluster the cut would select, read
   `resident[id]`. Resident → emit (slot index, not `first_index`) + bump
   `slot_meta[slot].last_used_frame` (LRU touch). Not resident → (a) walk up to the
   nearest resident ancestor and emit THAT (crack-free coarse fallback = Step 1's
   clamp, but the "frontier" is wherever residency currently reaches), and (b)
   `atomicOr`/append `id` into a **`feedback`** buffer ("wanted but absent").
2. **Compaction (unchanged shape).** Packs selected slots' indices into the
   compacted stream + draw args; indices are now `slot*PAGE_VERTS + k` into
   `page_pool` rather than a contiguous `M`.
3. **Readback (async, amortized).** Copy `feedback` → MAP_READ (existing pattern),
   one frame latent. No per-frame stall: the draw used the coarse fallback this
   frame; the finer page appears a frame or two later (progressive refinement, like
   virtual texturing).
4. **Stream + evict (CPU).** For each wanted id: take a free slot or evict the oldest
   `last_used_frame` (skip slots used this frame). `writeBuffer` the cluster's
   exploded geometry into the slot, set `resident[id]=slot`. Cap uploads/frame to a
   byte budget so a big camera jump doesn't hitch. Clear evicted ids to `-1` first so
   the cut can't read a half-evicted slot.

**Crack-free.** The cut still only emits a valid DAG antichain (same group-consistent
`lod_bounds` tiling as Step 1); the coarse fallback when a page is absent is itself a
valid coarser antichain, so transitions stay watertight and just refine over a few
frames. The resident-leaf `lod_error→0` clamp from Step 1 becomes dynamic (applied to
whichever clusters are currently the finest resident on each path).

**Why it stays in budget.** Working-set = sum of the visible cuts' pages, not the
whole asset; cold pages evict. Pool size is the hard VRAM cap; detail degrades
gracefully (coarser) when saturated rather than overflowing — Step 1's property,
now camera-adaptive.

**Build order (each gated behind `cluster_streaming` or a new `cluster_paging`):**
1. `page_pool` + `resident[]` table + port the cut to read `resident` and emit slot
   indices — **static residency through the pool** (no feedback yet), proving the
   indirection renders identically to Step 1.
2. Add the `feedback` buffer + readback + CPU upload-into-slot (**grow-only**, no
   eviction) — camera-close now refines past the initial residency.
3. LRU eviction + per-frame upload byte budget.
4. Multi-million-tri on-device verification + `?stress=N` (no per-frame heap allocs;
   pool/reuse the readback + upload staging).

**Ties into** `PERFORMANCE_OPEN_WORLD_PLAN.md` — the VRAM-budget / LRU machinery is
shared with texture streaming.

---

## Gap C — minor polish (mostly subsumed by Gap B)

- **Uncapped positions.** Step 1 caps `M`'s exploded buffer (the dominant cost) but
  still uploads all `cm.positions`. For multi-million-*vertex* assets, cap/compact
  positions to the resident set too. Gap B's page pool handles this naturally
  (positions live per-slot).
- **Aggressive-cap frontier seams.** Under a very low static budget the partial
  frontier can seam where it borders coarser-only regions. Gap B's dynamic frontier
  removes the static partial frontier. If a static-only mitigation is wanted sooner,
  make Step 1's resident set a *complete* sub-DAG to an error threshold (whole
  frontier level) rather than a hard tri count — watertight, soft budget.

---

## Acceptance / verification for the remaining work

- **Gap A:** the subdivided sphere (and any `watertight:false` mesh) renders
  watertight at full detail and under `?streambudget`; a bake unit test pins
  DAG tiling + level-0 reconstruction on non-watertight input; real-asset output
  unchanged.
- **Gap B:** a genuinely multi-million-tri asset renders at interactive rates with
  full detail near the camera and bounded VRAM; panning/dollying refines without
  cracks; `?stress=N` shows no per-frame heap allocs (`?trace=sub-frame`).
- **Final multi-million-tri benchmarking (REQUIRED once Gap A + Gap B both land — the
  empirical multi-million-tri proof).** Build real test scenes in the genuine
  multi-million-triangle range and capture REAL numbers (don't just assert it):
  - **Scenes:** (a) one high-density *unique* asset — a sculpted/subdivided mesh of
    ≥5–10M source triangles (use the `Subdivide` modifier or import a dense scan;
    once Gap A lands, subdivided meshes bake watertight); (b) a heavily-*instanced*
    scene with *many distinct* multi-M-tri datasets, to exercise streaming residency
    + eviction (instancing shares one dataset, so distinct datasets are what stress
    VRAM — see the note below).
  - **Measure at 1080p and 4K, via `?trace=sub-frame`:** total frame time + per-pass
    breakdown (cut, compaction, geometry/vis-buffer, deferred shading); the **drawn
    triangle count (cut size) vs the source triangle count** (the headline: draw
    stays ~screen-bounded while source scales up); page-pool occupancy + eviction
    churn while dollying; peak VRAM. Confirm no per-frame heap allocs under
    `?stress=N`.
  - **Baseline:** compare against flags-off / non-LOD where the asset can even load,
    and note the multi-million-tri cases that *only* load because of streaming.
  - **Record the resulting table in this doc** as the closing evidence.
- **Always:** flags default-off ⇒ byte-identical (the non-LOD hot path is gated at
  `render.rs` `(!lod && !virtual_geometry) || lod.is_empty()` and `cluster_lod:
  Option`); `cargo test -p awsm-renderer -p awsm-renderer-materials -p
  awsm-renderer-scene-loader --lib` (+ `awsm-renderer-lod-bake` when touching the
  bake) green; `cargo fmt` + `task lint` clean; on-device self-verify before commit.

---

## Settled decision — software rasterizer is **NO-GO** (do not re-litigate)

Nanite compute-rasterizes sub-pixel triangles with a 64-bit `atomicMin(depth<<32 |
payload)` (one atomic that depth-tests + records the winner). **WebGPU has no 64-bit
atomics**, so the whole SW-raster win hinges on emulating that atomic well enough to
beat HW raster for tiny triangles. A self-contained WebGPU bench (own `GPUDevice`,
run via chrome-devtools `evaluate_script`) measured it on the dev Apple GPU:

- Atomic-throughput baseline: **~5.75 G `atomicMax`/s**.
- HW vs **Encoding A** (packed-u32 `atomicMax` SW raster — the *perf ceiling*; 16-bit
  payload, unusable in production), three runs sweeping triangle size: A beats HW
  **at best ~1.5–1.8× and only at sub-pixel (≤1px)** sizes — within the sub-ms
  measurement noise — and **loses at ≥2px**. Chrome wall-clock is noisy at sub-ms and
  timestamp-queries quantize to 100 µs, but the *shape* is stable across all runs.
- Since A is the unusable ceiling and the production **Encoding B** (emulated-64
  depth+payload via a CAS spin) is strictly slower, the "worthwhile margin" bar isn't
  met. HW raster on this target is efficient even for tiny triangles.

⇒ **Phase 3 (hybrid SW/HW raster) is not built.** HW-raster cluster-LOD is the
end-state. **Re-open condition:** only if a *future target* shows HW raster as a
*proven* sub-pixel bottleneck — then re-run the bench (appendix) with a heavier
controlled workload (≥50 ms) to beat the noise before reconsidering.

Bit-budget note (why one u32 can't be the production encoding): payload must identify
the winning surface (triangle-within-cluster ~7 bits + visible-cluster index ~18
bits ≈ 25 bits), leaving only ~7 depth bits in a u32 — z-fighting. Hence the
emulated-64 Encoding B, which the ceiling result already rules out as worth building.

### Appendix — SW-raster bench harness (kept for the re-open condition)

Run via chrome-devtools `evaluate_script` (its own device; no editor interference).

```js
// Atomic-throughput baseline (Step 1): ~5.75 G atomicMax/s on the dev Apple GPU.
async function atomicThroughputProbe() {
  const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  const dev = await adapter.requestDevice();
  const W = 256, H = 256, PIX = W * H;
  const fb = dev.createBuffer({ size: PIX*4, usage: GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_SRC|GPUBufferUsage.COPY_DST });
  const wgsl = `
    @group(0) @binding(0) var<storage, read_write> fb: array<atomic<u32>>;
    @compute @workgroup_size(64) fn main(@builtin(global_invocation_id) gid: vec3<u32>){
      let t = gid.x; var seed = t*747796405u + 2891336453u;
      for (var k=0u;k<256u;k++){ seed = seed*747796405u + 2891336453u;
        atomicMax(&fb[seed % ${PIX}u], (((seed>>16u)&0xffffu)<<16u)|(t & 0xffffu)); }
    }`;
  const pipe = dev.createComputePipeline({ layout:"auto", compute:{ module: dev.createShaderModule({code:wgsl}), entryPoint:"main" }});
  const bg = dev.createBindGroup({ layout: pipe.getBindGroupLayout(0), entries:[{binding:0, resource:{buffer:fb}}]});
  const THREADS=65536, groups=THREADS/64;
  const run = () => { const e=dev.createCommandEncoder(); const p=e.beginComputePass(); p.setPipeline(pipe); p.setBindGroup(0,bg); p.dispatchWorkgroups(groups); p.end(); dev.queue.submit([e.finish()]); };
  run(); await dev.queue.onSubmittedWorkDone();
  const ITERS=50, t0=performance.now();
  for(let i=0;i<ITERS;i++) run(); await dev.queue.onSubmittedWorkDone();
  const ms=(performance.now()-t0)/ITERS, ops=THREADS*256;
  return { ms_per_iter:+ms.toFixed(3), Gatomics_per_sec:+((ops/(ms/1000))/1e9).toFixed(2) };
}
```

HW-baseline-vs-Encoding-A (the gate): build N triangles of a target pixel size
(pos f32x3 + payload u32, 16-byte stride); HW = a render pipeline writing payload to
an `r32uint` target with reverse-Z depth (`depthCompare:"greater"`); A = a
`@workgroup_size(64)` compute, one thread/triangle, bbox scan + edge test, per
covered pixel `atomicMax((u32(depth*65535)<<16)|payload16)` into `array<atomic<u32>>`;
time each over ~200 iters via `queue.onSubmittedWorkDone()`. Use a **fixed moderate
N** (~1M tris, ~32 MB buffer) — variable-N huge buffers (>100 MB) break and sub-ms
wall-clock is noise-dominated; for a credible re-run on a new target, scale the
workload so each pass is ≥50 ms. (Reverse-Z + `atomicMax` = closest-wins, provided
depth sits in the high bits — no convention change.)
