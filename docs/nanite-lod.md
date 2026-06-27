# Level of Detail (LOD) & Virtual Geometry

Built-in LOD lets the renderer draw the *right amount of geometry* for how much of
the screen a mesh actually covers: full detail up close, progressively coarser with
distance. It is a property of **geometry**, independent of the material model — a
mesh shades the same whether it's drawn at full or reduced detail.

The renderer uses **two strategies, chosen automatically by mesh class**, because
the techniques that work for rigid geometry don't survive vertex deformation.

| Mesh class | LOD strategy | Detail varies… |
|---|---|---|
| **Static rigid** (`Mesh`, no skin/morph) | **Cluster virtual geometry** (Nanite-style) | *within* a single mesh, per-cluster, by distance |
| **Deforming** (skinned, or morph-target) | **Discrete LOD chain** | per *instance*, whole-mesh, by distance |

Both feed the same visibility buffer and the same deferred material passes, so a
scene can freely mix static + skinned + morph meshes at different detail levels in
one frame.

---

## The runtime pipeline, and where "hardware" vs "software" rasterization fits

A common point of confusion: *everything* here runs on the GPU, and there's an
offline bake too — so where does "hardware vs software rasterizer" even apply? It
applies to exactly **one step — rasterization** (turning a projected triangle into
the pixels it covers) — and it is **not** a GPU-vs-CPU distinction.

| Step | When / where | What it decides |
|---|---|---|
| **Bake** | offline, at editor export | *What* geometry exists at each detail level (the cluster hierarchy / discrete levels) |
| **Cut** | runtime, GPU compute | *Which* clusters/levels to draw this frame |
| **Compaction** | runtime, GPU compute | Packs the selection into one indirect draw stream |
| **Rasterize** | runtime, GPU | Turns those triangles into visibility-buffer pixels — **the only HW/SW choice** |
| **Shade** | runtime, GPU compute, deferred | Material, per pixel |

- **Hardware rasterization** = the GPU's **fixed-function rasterizer** (dedicated
  silicon). We hand it triangles via `drawIndexedIndirect`; it computes coverage,
  runs the fragment shader, does early-Z. **This is what we use.**
- **Software rasterization** = reimplementing coverage yourself in a **compute
  shader** (bounding-box scan + edge tests + an atomic write per covered pixel). It
  still runs on the GPU — "software" only means *you* wrote the rasterizer instead
  of using the fixed-function unit.

The fixed-function rasterizer always shades in 2×2-pixel quads (needed for
derivatives), so a sub-pixel triangle still lights a full quad — up to ~4× wasted
fragment work. A compute rasterizer can write exactly the covered pixels, which is
why engines like Nanite use it for sub-pixel triangles. **We don't:** on our targets
the hardware rasterizer is efficient enough that the cost isn't worth the complexity
(WebGPU has no 64-bit atomics, so a faithful compute rasterizer must emulate the
depth+payload atomic), and the LOD cut deliberately keeps triangles **near
pixel-sized rather than sub-pixel** — the regime where hardware rasterization is
fine. The bake is orthogonal to all of this: it decides *what* triangles can be
selected, never *how* they're rasterized.

Everything except rasterization is compute (cut, compaction, shading) or offline
(bake). Rasterization is hardware.

---

## Static meshes — cluster virtual geometry

A static mesh is baked into a **cluster DAG**: the mesh is split into small clusters
(≤128 triangles), groups of clusters are simplified into coarser parent clusters,
and that repeats up to a few root clusters. Every cluster records the screen-space
error it introduces and a group-shared bounding sphere.

Each frame, a GPU **cut** walks this DAG and selects, **per cluster**, the coarsest
version whose projected error still fits a pixel budget. Because each cluster uses
its *own* distance to the camera, **detail varies within a single mesh** — the near
side of a large object stays fine while the far side coarsens, in one draw.

- **Crack-free.** Clusters that were simplified together share a bounding sphere and
  flip detail levels at the same camera threshold, so adjacent clusters never
  disagree at a seam. The selection is always a valid "antichain" through the DAG.
- **One indirect draw.** The cut's selected clusters are compacted into a single
  index stream and drawn with `drawIndexedIndirect` into the shared visibility
  buffer — no per-cluster draw calls, no CPU per-frame work.
- **Occlusion-culled first.** A two-level hierarchical-Z occlusion cull removes
  off-screen and hidden clusters before the draw.

### Streaming residency (how multi-million-triangle assets fit)

The geometry the GPU must hold is bounded by a **residency budget**, not the asset
size. Cluster geometry lives in a fixed-capacity **page pool**; each frame the cut
marks which finer pages it wants near the camera, the CPU streams those in and
**evicts cold pages (LRU)** against the budget, and where a wanted page isn't yet
resident the cut falls back to the nearest resident (coarser) ancestor — still
crack-free — and refines over the next frame or two, like virtual texturing.

The result is two independent bounds, both on the hardware rasterizer:

- **Bounded draw:** the cut's triangle count tracks **screen resolution + the
  pixel-error budget, not the source size** — the same draw whether the source is
  1M or 500M triangles (≈ a few hundred thousand to ~2M triangles for typical
  resolutions). It's cheap because the renderer is visibility-buffer deferred: the
  geometry pass only writes triangle-ID (a thin raster pass), and material shading
  is deferred and **per-pixel** (bounded by screen pixels, not triangle count).
- **Bounded VRAM:** residency holds only the working set (the visible cuts' pages);
  detail degrades gracefully to coarser rather than overflowing when the budget is
  saturated.

The residency budget is configurable; small meshes that fit are uploaded whole, so
streaming only engages for assets that would otherwise exceed the budget.

---

## Deforming meshes — discrete LOD chain

Skinned and morph-target meshes **can't** use the cluster DAG: the cluster bounds
and the boundary-locked simplification are precomputed in object space assuming the
vertices don't move, but a skinned/morphing mesh repositions its vertices every
frame — which would invalidate the per-cluster error bounds and tear the crack-free
seams.

Instead, the bake produces a short **chain of whole-mesh levels** (`level 0` = the
original, then progressively simplified). Crucially, the simplifier only ever
**removes** vertices, so each level is a strict subset of the original — the bake
**carries the skin weights and morph targets through** to the surviving vertices.
A reduced level therefore still skins and morphs exactly like the original, just
with fewer vertices.

At runtime each **instance** selects **one** level by projected screen-space error
and the draw is rerouted to that level's mesh. Selection is a cheap per-instance
visibility swap folded into the existing cull/compaction — no extra passes.

(Static meshes also have a discrete chain available; it's the fallback path when a
build runs without the virtual-geometry feature. When virtual geometry is enabled,
static meshes use the cluster path described above.)

---

## The bake (export-time)

LOD data is precomputed **once, at editor export** — never at load — and
content-hash cached, so re-exporting an unchanged mesh skips the work. The bake is
pure-Rust (it runs in the same wasm toolchain as the editor; it does not depend on
native mesh libraries), and it operates on the already-final geometry:

- **Discrete levels:** a boundary-locked quadric (QEM) simplification to N levels,
  remapping skin weights + morph targets to the survivors.
- **Cluster DAG:** clustering → grouping → boundary-locked simplify → regroup, with
  per-group monotonic error and group-shared bounds (what makes the runtime cut
  crack-free). Robust to non-watertight and procedurally-subdivided input.

Meshes below a small triangle floor, or with LOD disabled (below), are skipped.

---

## Toggling LOD

There are two levels of control:

**1. Per-mesh, in the editor (content).** Each mesh has a **LOD toggle** in its
inspector (alongside the shadow toggles), persisted in the project. It is
**opt-out / default-on**: meshes get LOD baked unless you explicitly turn it off
for a specific mesh (e.g. a hero asset you always want at full detail). The same
toggle is scriptable via the `set_mesh_lod` MCP tool. With it off, that mesh is
baked and drawn whole, and a per-instance override can also pin an individual
instance to full detail.

**2. Per-build, the renderer feature gates (runtime).** The player runtime enables
LOD through renderer feature flags — discrete LOD, cluster virtual geometry, and
cluster streaming are each separately gateable. **When a gate is off, that
subsystem is byte-identical to a build without it**: no level/cluster data is
loaded, no per-frame selection runs, and every instance draws its base mesh. This
is what guarantees zero cost (see below) when LOD isn't wanted. During development
these gates are exposed as URL flags on the editor's player preview.

---

## Tradeoffs & costs

- **When LOD is off, it costs nothing.** All LOD/cluster/streaming work is gated; the
  non-LOD hot path is unchanged and incurs no extra dispatches or allocations. The
  only always-present footprint is a small per-mesh config field (CPU state, not the
  render loop).
- **When LOD is on, it's generally a win where it matters.** Discrete LOD drops
  triangle counts substantially at mid/far distance; cluster LOD draws only the
  visible cut of a large static mesh. The fixed per-frame cost is a couple of small
  compute dispatches (cut + compaction) plus the per-instance selection — like
  occlusion culling, that overhead is amortized when there's real geometry to
  reduce, and is roughly neutral (never a large regression) for trivial scenes.
- **No per-frame heap allocations** in the render hot path — selection, streaming
  readback, and upload staging are pooled/reused.
- **Quality knob:** the pixel-error budget trades detail for triangle count. Looser
  budgets shrink the cut (faster) and keep triangles comfortably above the
  sub-pixel range; tighter budgets approach one-triangle-per-pixel, where the
  hardware rasterizer's 2×2-quad granularity starts to waste fragment work — the
  point at which a compute software rasterizer would become worth revisiting.
- **Streaming latency:** newly-revealed detail can take a frame or two to page in
  (progressive refinement), shown as a brief coarser-then-sharper transition rather
  than a stall.

---

## Summary

- Static rigid meshes use **cluster virtual geometry**: a per-cluster GPU cut over a
  baked DAG draws only the visible, distance-appropriate detail of a mesh in one
  indirect draw, with crack-free seams, and a streaming residency pool keeps VRAM
  bounded so multi-million-triangle assets fit.
- Deforming (skinned / morph) meshes use a **discrete LOD chain** of whole-mesh
  levels selected per instance, because the cluster technique assumes static
  geometry.
- All rasterization is on the **hardware rasterizer**; the cut, compaction, and
  shading are GPU compute; the bake is offline. The draw cost is bounded by screen
  resolution, and the resident geometry by a VRAM budget.
- LOD is **opt-out per mesh** in the editor and **feature-gated per build**, with
  flag-off being byte-identical to a renderer without LOD.

---

## Tooling & integration

### Offline pre-bake — `awsm-renderer-lod-bake` CLI
Baking a heavy mesh in the browser is slow and can exceed GPU buffer limits. The
`awsm-renderer-lod-bake` binary (crate `awsm-renderer-lod-bake-cli`, in `packages/tools/lod-bake-cli`)
converts a glTF/GLB **offline** into nanite-ready assets, reusing the exact crates
the editor's export bake uses (so output is identical to an in-editor bake).

Install it the same way as the MCP server — prebuilt binaries on GitHub Releases
(driven by cargo-dist), or from source:

```
# macOS / Linux (prebuilt)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-renderer-lod-bake-cli-installer.sh | sh
# Windows (PowerShell, prebuilt)
powershell -ExecutionPolicy Bypass -c "irm https://github.com/awsm-fun/awsm-renderer/releases/latest/download/awsm-renderer-lod-bake-cli-installer.ps1 | iex"
# From source (needs Rust)
cargo install --git https://github.com/awsm-fun/awsm-renderer awsm-renderer-lod-bake-cli
```

Then run it:

```
awsm-renderer-lod-bake my-model.glb --out ./assets
```

Per mesh node it writes `<id>.glb` (base), `<id>.lod{N}.glb` + `<id>.lod.toml`
(discrete chain), and `<id>.clusters.bin` (the cluster DAG, JSON). The DAG builder
welds coincident positions for adjacency (`DagOptions::weld_eps`) so split-vertex
glTF (UV/normal seams) clusters cleanly instead of degenerating to ~1 tri/cluster.

### Editor — view-only nanite import
A pre-baked asset imports into the editor as a **view-only** `NodeKind::ClusterMesh`
(a third geometry category alongside `Mesh` and `SkinnedMesh` — not editable; it IS
the LOD). It renders through the **same cluster pipeline the player uses**
(`scene-loader::materialize_cluster_mesh`) — no in-editor re-baking and no dense
visibility-geometry explode — so a multi-million-triangle mesh views as nanite,
bounded, without crashing the editor. Drive it with the `import_nanite_asset` MCP
tool (or `EditorCommand::ImportNaniteAsset { clusters_url }`). The editor enables
`virtual_geometry` + `cluster_paging` by default for this (escape: `?novg` /
`?nopaging`); per-frame cost stays zero for scenes with no resident cluster mesh.

### Build & runtime gating
- **Compile-time:** `lod` is a default-on Cargo feature on `awsm-renderer` (+
  `awsm-renderer-scene-loader`). Build with `default-features = false` and ALL LOD
  code is `#[cfg]`-compiled out — modules, the cluster render pass, the per-frame
  cut/paging/selection, the scene-loader load paths, and the `lod-bake` dependency.
- **Runtime:** the `RendererFeatures` flags (`lod`, `virtual_geometry`,
  `cluster_streaming`, `cluster_paging`) on `AwsmRendererBuilder::with_features`
  gate work per renderer; a scene with no LOD meshes pays nothing per frame (the cut
  early-outs at `cluster_count == 0`, paging early-returns with no resident mesh).

## Status & verification
All six headline acceptance claims (crack-free per-cluster cut incl. non-watertight;
multi-million-tri streaming residency; cut bounded by screen not source; deforming →
discrete chain; flags-off byte-identical; the benchmark) are **shipped + verified**
with committed tests + on-device evidence. The multi-M benchmark is recorded in
[`nanite-lod-benchmark.md`](./nanite-lod-benchmark.md) (a 1,081,344-tri source /
2,393,468-tri DAG → ~83 MB bounded pool, M capped to 29,850 tris, cut 4.9k–14.8k tris
scaling with viewport height) and pinned by the `a6_benchmark_table_recorded` test.

Editor cluster-asset persistence is **shipped**: a view-only nanite import survives
Save→reload and ships in the player bundle. `persistence::cluster_files` writes each
referenced DAG to `assets/<source>.clusters.bin` (from the session-local
`cluster_cache`, keyed by `AssetId` — not by re-fetching the import URL, so even a
local `blob:`-URL import persists), and `restore_cluster_meshes` re-reads it into the
cache before the scene materialises, in all three load paths.

Degenerate / pathological-topology robustness is **shipped**: the degeneracy verdict
is one shared heuristic (`ClusterMesh::quality`) used by BOTH the offline CLI and the
editor export bake, so a mesh that won't cluster (non-manifold / unweldable) drops its
cluster DAG and falls back to the discrete LOD chain instead of shipping a hole-prone
one; the simplifier locks non-manifold edges (≥3-incidence) so they can't collapse
asymmetrically; and `ClusterMesh::validate` rejects a malformed `.clusters.bin` at load
rather than reading out of bounds. Crack-free coverage spans the UV sphere (A1) and a
genus-1 torus.

**Known follow-ups (not regressions):** (1) one cluster render mesh is resident at a
time (multiple simultaneous nanite meshes is future work). See
[`plans/nanite-follow-up.md`](./plans/nanite-follow-up.md).
