# Cluster-LOD / Virtual-Geometry — Acceptance Checklist

> **Purpose.** This file is the binding acceptance contract that brings the
> implementation up to the permanent north-star spec [`docs/nanite-lod.md`](../nanite-lod.md).
> The north star describes the *finished* system as fact; this file enumerates
> **every behavioral claim** it makes and states **how each is verified**. A box is
> ticked **only** when verified by a committed test **and/or** cited on-device
> evidence (browser-console GPU readback / screenshot), per the loop contract.
>
> Implementation guide: [`docs/plans/nanite-software-rasterize.md`](./nanite-software-rasterize.md)
> (Gap A bake/cut robustness → Gap B dynamic paging → Gap C polish → benchmark).
>
> **Loud-failure rule:** if any box is unmet at a stopping point, commit
> `docs/plans/nanite-lod-NORTHSTAR-GAPS.md` and add a failing / `#[ignore]`d test
> encoding the gap. Never present a green suite that hides an unmet claim.

Status legend: `[ ]` unmet · `[~]` partial / shipped-but-not-re-verified-in-this-loop · `[x]` verified.

---

## Mandated headline claims (A1–A6)

- [ ] **A1 — Per-cluster cluster cut, crack-free, incl. non-watertight / subdivided.**
  Static rigid meshes render via the per-cluster GPU cut with no cracks, *including*
  non-watertight / midpoint-subdivided meshes. The subdivided-sphere repro
  (`meshgen sphere` + `Subdivide×4`, ~262k→550k-DAG-tris) renders **watertight at
  full detail** (`?vg`, no cap) **and** when capped (`?streambudget=N`).
  **Verify:** (1) committed bake unit test in `awsm-renderer-lod-bake` that bakes a
  known non-watertight mesh and asserts the DAG error-intervals **tile** `[0,∞)` on
  every root→leaf path **and** level-0 clusters **reconstruct** the source triangles
  (`base_triangle_count` invariant). (2) On-device: subdivided-sphere screenshot
  watertight at orbit radius ~2, both uncapped and `?streambudget`; cut-coverage
  GPU readback shows every surface region has a selected cluster. Real-asset
  (DamagedHelmet) output **unchanged** (regression guard).
  _Current: **bake-level fix LANDED + test PASSING.** `simplify::weld_coincident`
  position-welds the simplifier's topology (a seam/pole duplicate becomes an interior
  edge, not a false open boundary) and a new `lock_boundaries` mode (used by the
  cluster-DAG group simplify via `with_target_locked`) fully locks the remaining true
  boundaries so adjacent groups can't slide a shared boundary apart. The reproduction
  test `non_watertight_sphere_cut_is_closed_at_every_level` now passes: 0 hole edges
  at every cut threshold AND a real reduction (coarsest cut < ¾ source). Dup-free
  watertight meshes weld to identity ⇒ real-asset output unchanged. **A1 still
  UNTICKED** pending on-device confirmation of the actual editor subdivided sphere
  under `?vg` (full detail) and `?streambudget` (capped)._

- [ ] **A2 — Dynamic, camera-driven streaming residency.**
  A genuinely multi-million-tri asset renders full detail near the camera within a
  **bounded VRAM** budget; finer pages **stream in** near the camera and **evict
  (LRU)** when cold; absent pages fall back to the nearest resident coarser ancestor
  **crack-free** and refine over a frame or two; **no per-frame heap allocs** in the
  hot path.
  **Verify:** (1) On-device with a multi-M-tri scene + `?stream`/paging flag:
  dollying in refines detail (screenshots before/after); page-pool occupancy stays
  ≤ budget; eviction churn observed via console readback; coarse-then-sharp
  transition is crack-free (no seam/hole screenshots). (2) `?stress=N` +
  `?trace=sub-frame`: no per-frame heap allocations attributable to the paging path
  (pooled readback + upload staging). (3) Committed test(s) for the
  residency table / LRU eviction / slot-recycle logic (CPU-side, scene-loader/renderer).
  _Current: only Step 1 static cap shipped; dynamic per-frame paging absent → unmet (Gap B)._

- [ ] **A3 — Drawn (cut) triangle count bounded by screen resolution, not source size.**
  The cut's drawn-triangle count tracks screen resolution + pixel-error budget, and
  stays ~flat as the source scales into the millions.
  **Verify:** benchmark table (A6) shows cut size ≈ constant across sources of
  ~1M / ~10M / (streamed) larger at a fixed resolution + error budget; plus a
  console-readback of `selected`/drawn-index count at two source densities.
  _Current: depends on A2 (true multi-M source needs paging) → unmet._

- [ ] **A4 — Deforming meshes use the discrete chain, per-instance, skin/morph carried.**
  Skinned / morph-target meshes use the discrete LOD chain selected **per instance**;
  skin weights + morph targets are carried through to the simplified levels (each
  level is a strict vertex subset that still skins/morphs like level 0). A mixed
  static + skinned + morph scene renders all three at distance-appropriate detail in
  one frame, all feeding the shared visibility buffer.
  **Verify:** (1) committed bake test: simplified discrete level is a vertex subset
  with remapped skin weights + morph targets present and consistent. (2) On-device
  mixed-scene screenshot at near/far distances showing per-instance level swaps with
  skinning/morph intact; console readback of selected level per instance.
  _Current: shipped Phase A; re-verify on-device in this loop._

- [ ] **A5 — Flags off ⇒ byte-identical (no non-LOD regression).**
  With `lod` / `virtual_geometry` / `cluster_streaming` (and any new paging flag) all
  off, the renderer is byte-identical to a build without LOD: no level/cluster data
  loaded, no per-frame selection dispatch, every instance draws its base mesh.
  **Verify:** (1) committed test asserting feature defaults are all `false` and the
  gated paths are skipped when off (extends `features.rs` default-off assertions).
  (2) On-device: flags-off render of a reference scene matches the pre-change render
  (screenshot diff / identical frame); no extra dispatches in `?trace=sub-frame`.
  (3) `cargo test` baseline stays green with new subsystems compiled-in but gated.
  _Current: shipped invariant exists; re-verify per new flag added._

- [ ] **A6 — Required final multi-million-tri benchmark TABLE recorded in the docs.**
  A real-numbers table at **1080p and 4K**: total frame time + per-pass breakdown
  (cut, compaction, geometry/vis-buffer, deferred shading); **cut size vs source
  size**; page-pool occupancy + eviction churn while dollying; **peak VRAM**;
  baseline vs flags-off where loadable; note cases that *only* load via streaming.
  **Verify:** table committed into `docs/plans/nanite-software-rasterize.md`
  (Acceptance section) with captured numbers, not assertions.
  _Current: blocked on A1+A2 landing → unmet._

---

## Derived supporting claims (the rest of docs/nanite-lod.md)

These are also behavioral claims in the north star; they must hold for A1–A6 to be
honestly true. Grouped; each cites its verification.

### Bake (export-time)
- [ ] **B1** Bake runs offline at editor export, never at load; content-hash cached
  (re-export of unchanged mesh skips work). _Verify: existing cache test +
  cite bake-skip path; no load-time bake in scene-loader._
- [ ] **B2** Bake is pure-Rust on the wasm toolchain (no native mesh libs —
  no meshopt/metis). _Verify: `cargo tree -p awsm-renderer-lod-bake` shows no
  meshopt/metis; crate builds for wasm32._
- [ ] **B3** Cluster DAG bake: ≤128-tri clusters → group → boundary-locked simplify →
  regroup; per-group **monotonic error** + **group-shared bounds**; robust to
  non-watertight + subdivided input. _Verify: covered by A1 bake test + existing
  dag.rs tests; add monotonic-error + group-shared-bounds assertions on
  non-watertight input._
- [ ] **B4** Discrete bake: boundary-locked QEM to N levels, remapping skin weights +
  morph targets to survivors. _Verify: covered by A4 bake test._
- [ ] **B5** Meshes below a triangle floor, or with LOD disabled, are skipped.
  _Verify: committed test on the floor/disabled skip path._

### Runtime cut / draw
- [ ] **B6** GPU cut selects, per cluster, the coarsest version whose projected error
  fits the pixel budget; detail varies within one mesh. _Verify: on-device readback
  (near side fine / far side coarse on one large mesh) + cut-count log._
- [ ] **B7** Selection is always a valid DAG antichain; clusters simplified together
  flip at the same threshold (crack-free seams). _Verify: subsumed by A1; antichain
  property asserted in a cut-logic test if feasible CPU-side._
- [ ] **B8** One indirect draw: compacted into a single index stream via
  `drawIndexedIndirect`, no per-cluster draw calls, no CPU per-frame work.
  _Verify: cite `cluster_compaction.wgsl` + single indirect draw in render.rs; trace
  shows one draw._
- [ ] **B9** Two-level hierarchical-Z occlusion cull removes hidden clusters before
  the draw. _Verify: cite occlusion pass; readback occluded-cluster count > 0 in a
  scene with occlusion._

### Routing / class selection
- [ ] **B10** Strategy chosen automatically by mesh class (static rigid → cluster VG;
  skinned/morph → discrete chain) via `node_is_skinned`. _Verify: covered by A4
  mixed-scene; cite routing in scene-loader/mesh.rs._
- [ ] **B11** Static meshes also have a discrete chain as the fallback path when
  `virtual_geometry` is off. _Verify: with `lod` on + `virtual_geometry` off, a
  static mesh uses discrete levels (on-device + cite)._

### Editor / content control
- [ ] **B12** Per-mesh LOD toggle in the inspector (alongside shadow toggles),
  persisted in the project; opt-out / default-on. _Verify: cite inspector UI +
  project persistence; toggle round-trip via `editor_query_json`/`editor_dispatch_json`._
- [ ] **B13** Toggle scriptable via `set_mesh_lod` MCP tool; off ⇒ mesh baked + drawn
  whole; per-instance override can pin an instance to full detail. _Verify: drive
  `set_mesh_lod` (cite `packages/mcp/src/mcp.rs`) + on-device effect._

### Costs / invariants
- [ ] **B14** No per-frame heap allocations in the render hot path (selection,
  streaming readback, upload staging pooled/reused). _Verify: subsumed by A2
  `?stress=N`; code audit of new paging path for per-frame `Vec`/`Box` allocs._
- [ ] **B15** When LOD on for trivial scenes, overhead is roughly neutral (couple of
  small compute dispatches + per-instance selection), never a large regression.
  _Verify: `?stress` A/B on a trivial scene, flags on vs off, within noise._
- [ ] **B16** All rasterization on the hardware rasterizer; cut/compaction/shading are
  GPU compute; bake is offline. _Verify: cite pipelines (HW raster geometry pass;
  compute cut/compaction) — consistent with settled SW-raster NO-GO._

---

## Progress log
- 2026-06-25 — checklist derived from `docs/nanite-lod.md` and committed.
  Current truth: Gap A (A1) reproduced-bug → unmet; Gap B (A2/A3) absent → unmet;
  A6 blocked on A1+A2; A4/A5 shipped, pending in-loop re-verification. **0/6 headline
  claims verified in this loop.**
- 2026-06-25 — Gap A **reproduced deterministically at the bake level** (no GPU):
  added `non_watertight_sphere_cut_is_closed_at_every_level` (`#[ignore]`d, A1 gap).
  Root cause localized: index-based adjacency treats coincident seam/pole duplicates
  as open boundaries → coarse-level simplify tears holes (21 hole edges at the first
  coarse threshold). Diagnosed via `meshgen::sphere_mesh` topology (duplicated seam
  column + pole rows; `subdivide` preserves it). Next: position-aware bake fix +
  un-ignore. **Still 0/6 headline verified.**
- 2026-06-25 — Gap A **bake fix landed**: `simplify::weld_coincident` (position-weld
  topology) reduced the tear 21→6; the residual 6 were cross-group cracks from the
  "Boundary slide" rule, fixed by a `lock_boundaries` simplify mode the cluster DAG
  uses (`with_target_locked`) — safe now because welding already turned attribute
  seams interior, so locking the remaining true boundaries no longer re-triggers the
  seam plateau. Test un-ignored and passing (0 holes at every cut level + real
  reduction). Bake/renderer suites green (33/301/34/36), fmt + clippy clean. A1 tick
  pending on-device. **Still 0/6 headline verified** (no tick without on-device).
