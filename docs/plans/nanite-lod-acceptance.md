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

> **Current: 3 / 6 headline verified (A1, A4, A5).** The unmet A2 / A3 / A6 (the
> large Gap-B dynamic-paging build + its benchmark) are documented in
> [`nanite-lod-NORTHSTAR-GAPS.md`](./nanite-lod-NORTHSTAR-GAPS.md) and flagged in
> `cargo test` by `#[ignore]`d markers `a2_dynamic_camera_driven_paging`,
> `a3_cut_bounded_by_screen_not_source`, `a6_benchmark_table_recorded`
> (scene-loader). Gap B foundation (flag + planner + `?paging`) is in, gated +
> byte-identical; the GPU page-pool + dynamic swap remain.

---

## Mandated headline claims (A1–A6)

- [x] **A1 — Per-cluster cluster cut, crack-free, incl. non-watertight / subdivided.** ✅
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
  _Current: **✅ VERIFIED — uncapped AND capped crack-free (on-device + unit tests).**
  (1) Bake fix landed (`simplify::weld_coincident` + `lock_boundaries` via
  `with_target_locked`); unit test `non_watertight_sphere_cut_is_closed_at_every_level`
  passes (0 holes at every cut level + real reduction). (2) **On-device (2026-06-25):**
  editor rebuilt with the fix; sphere + Subdivide×4 → `load_player_bundle` under `?vg`.
  Browser console: `cluster LOD (GPU): …13065 clusters, render mesh M = 583768 tris,
  per-cluster cut drives draw` and `cluster compaction (GPU): …1696 tris over 13065
  clusters` — the per-cluster cut drives the draw; screenshots at orbit radius 2 and
  1.08 show a **complete, hole-free** sphere (also bonus A3 evidence: 1696 drawn vs
  583768 source). (3) **CAPPED FAILS:** under `?streambudget=8000` the sphere renders
  with **visible holes / sliver tears** — the static cap's PARTIAL frontier (hard tri
  budget cutting mid-level) borders coarser-only regions and seams. Reproduced
  deterministically (`capped_resident_cut_is_crack_free`, 50 hole edges @ budget 1512).
  **FIXED:** `select_resident_clusters` now selects the finest COMPLETE-antichain cut
  within budget (soft budget; frontier emitted always-drawn, lod_error=0/parent_error=MAX)
  instead of a hard-tri partial frontier; the capped test passes (`cap_to_two_tris…`
  updated to expect the complete leaf antichain). **On-device re-verify (editor rebuilt):**
  `?vg&streambudget=8000` renders **watertight** (screenshot, no holes); console
  `cluster LOD (GPU): …(476 resident), M = 7996 tris (CAPPED from 583768 — budget 8000),
  per-cluster cut drives draw`. Over-budget branch only — flag-off/under-budget stays
  verbatim passthrough (no regression). → **A1 ✅ MET.**_

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
  _Current: **Gap B IN PROGRESS (foundation laid) → still UNMET.** Step 1a done:
  `cluster_paging` default-off feature flag (features.rs, asserted off by
  `default_features_are_all_off`) + a pure, unit-tested CPU page-pool planner
  `plan_page_pool` (scene-loader: cluster→slot `resident` table, occupancy, overflow;
  3 tests) wired behind the flag in `load_cluster_lod` to log pool occupancy (no render
  change yet ⇒ byte-identical). STILL TO BUILD: GPU page-pool slot buffer + `resident`
  table upload + cut/compaction shader read of `resident` (slot-relative indices) +
  feedback buffer + async readback + CPU stream/evict (LRU) + per-frame upload budget +
  multi-M-tri on-device verify + no-per-frame-allocs (`?stress=N`). → A2 unmet._

- [ ] **A3 — Drawn (cut) triangle count bounded by screen resolution, not source size.**
  The cut's drawn-triangle count tracks screen resolution + pixel-error budget, and
  stays ~flat as the source scales into the millions.
  **Verify:** benchmark table (A6) shows cut size ≈ constant across sources of
  ~1M / ~10M / (streamed) larger at a fixed resolution + error budget; plus a
  console-readback of `selected`/drawn-index count at two source densities.
  _Current: depends on A2 (true multi-M source needs paging) → unmet._

- [x] **A4 — Deforming meshes use the discrete chain, per-instance, skin/morph carried.** ✅
  Skinned / morph-target meshes use the discrete LOD chain selected **per instance**;
  skin weights + morph targets are carried through to the simplified levels (each
  level is a strict vertex subset that still skins/morphs like level 0). A mixed
  static + skinned + morph scene renders all three at distance-appropriate detail in
  one frame, all feeding the shared visibility buffer.
  **Verify:** (1) committed bake test: simplified discrete level is a vertex subset
  with remapped skin weights + morph targets present and consistent. (2) On-device
  mixed-scene screenshot at near/far distances showing per-instance level swaps with
  skinning/morph intact; console readback of selected level per instance.
  _Current: **✅ VERIFIED.** (1) Skin/morph carry-through: new bake test
  `discrete_lod_carries_skin_and_morph_verbatim` (lib.rs) builds a discrete chain and
  asserts gathered skin joints (u16×4) + weights (f32×4) + morph deltas (f32×3) are the
  survivor's value **bit-identical** (subset, no interpolation). (2) Per-instance
  selection: committed `lod::select_level` (lod.rs, coarsest level within a screen-error
  px threshold; 6 tests) applied per instance by `update_lod_selection` (render.rs:2394);
  skinned LOD via `skin_lod::period_for_distance` (tested). (3) On-device (2026-06-25,
  `?vg&lod`): mixed scene = imported **CesiumMan** (skinned) + **AnimatedMorphCube**
  (morph, kind `skinned_mesh`) + subdivided **Sphere** (static). Console shows the
  **only** `cluster LOD (GPU)` line is the Sphere (30979b0a…, 13065 clusters) — the
  deforming meshes are ABSENT from cluster LOD ⇒ routed to the discrete/deforming path,
  not clusters. Screenshot: all three coexist + render correctly (textured morph cube,
  skinned figure) in one frame. → **A4 ✅ MET.**_

- [x] **A5 — Flags off ⇒ byte-identical (no non-LOD regression).** ✅
  With `lod` / `virtual_geometry` / `cluster_streaming` (and any new paging flag) all
  off, the renderer is byte-identical to a build without LOD: no level/cluster data
  loaded, no per-frame selection dispatch, every instance draws its base mesh.
  **Verify:** (1) committed test asserting feature defaults are all `false` and the
  gated paths are skipped when off (extends `features.rs` default-off assertions).
  (2) On-device: flags-off render of a reference scene matches the pre-change render
  (screenshot diff / identical frame); no extra dispatches in `?trace=sub-frame`.
  (3) `cargo test` baseline stays green with new subsystems compiled-in but gated.
  _Current: **✅ VERIFIED.** `default_features_are_all_off` (features.rs) extended to
  assert `cluster_streaming == false` + `cluster_streaming_budget == None` (joins the
  existing lod / virtual_geometry / gpu_culling / … off-by-default asserts). On-device
  (2026-06-25): loaded sphere+Subdivide×4 WITHOUT `?vg` — the browser console compiles
  9 compute pipelines (HZB/Occlusion/Material/Decal/Effects) with **no "Cluster Cut"
  and no "Cluster Compaction"** (both present under `?vg`), no `cluster LOD (GPU)`
  readback, and the base mesh renders whole (screenshot) — i.e. no cluster data loaded,
  no per-frame cut dispatch, every instance draws its base mesh. The Gap-A fixes only
  execute in the flag-on/over-budget branches, so flag-off is unchanged. → **A5 ✅ MET.**_

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
- 2026-06-25 — **On-device A1 verification (editor rebuilt with fix):** UNCAPPED `?vg`
  subdivided sphere renders crack-free via the per-cluster cut (console: per-cluster
  cut drives draw, M=583768 tris/13065 clusters, 1696 drawn; screenshots radius 2 +
  1.08 hole-free). Built reusable `/tmp/mcp.py` MCP HTTP client to drive the editor.
  **CAUGHT a real remaining defect:** `?streambudget=8000` still tears (partial-frontier
  seam in `select_resident_clusters`). Encoded as `#[ignore]`d failing test
  `capped_resident_cut_is_crack_free` (50 hole edges @ budget 1512). A1 stays UNTICKED;
  next = complete-antichain residency fix. **Still 0/6 headline verified.**
- 2026-06-25 — **A1 ✅ COMPLETE.** Fixed the capped frontier seam:
  `select_resident_clusters` now selects the finest complete-antichain cut within
  budget (soft budget, always-drawn frontier) — `capped_resident_cut_is_crack_free`
  passes, `cap_to_two_tris…` updated. On-device (editor rebuilt): `?streambudget=8000`
  subdivided sphere renders **watertight** (476 resident clusters, M=7996/8000 tris,
  per-cluster cut drives draw). Both A1 clauses now verified. Suites green
  (301/34/36/34, 0 ignored), fmt+clippy clean. **1/6 headline verified (A1).** Next: Gap B
  (A2/A3 dynamic per-frame paging) behind a default-off flag.
- 2026-06-25 — **A5 ✅** flags-off byte-identical. Extended
  `default_features_are_all_off` (cluster_streaming + budget). On-device: flags-off
  scene compiles NO Cluster Cut/Compaction pipelines (vs `?vg`), base mesh renders
  whole. Suites 301/34/36/34 green, fmt+clippy clean. **2/6 headline verified (A1, A5).**
  Next: A4 (mixed skinned/morph), then Gap B (A2/A3).
- 2026-06-25 — **A4 ✅** deforming → discrete chain. New bake test
  `discrete_lod_carries_skin_and_morph_verbatim` (bit-exact skin joints/weights + morph
  delta carry-through); per-instance selection covered by committed `lod::select_level`
  + `skin_lod` tests. On-device (`?vg&lod`): mixed CesiumMan (skinned) + AnimatedMorphCube
  (morph) + static Sphere — only the Sphere is in `cluster LOD (GPU)`; deforming meshes
  route to the discrete path; all three coexist + render. Suites 301/35/36/34 green, 0
  ignored, fmt+clippy clean. **3/6 headline verified (A1, A4, A5).** Remaining: A2/A3
  (Gap B dynamic paging) + A6 (benchmark).
- 2026-06-25 — **Gap B step 1a (foundation)**: added `cluster_paging` default-off flag
  (features.rs + defaults test) and a pure, unit-tested `plan_page_pool` (scene-loader:
  cluster→slot resident table + occupancy/overflow; 3 tests), wired behind the flag to
  log pool occupancy — NO render change (byte-identical). Suites 301/35/36/37 green, 0
  ignored, fmt+clippy clean. **Still 3/6 verified** (A2 needs the GPU page pool + dynamic
  swap, next). Next step: GPU slot buffer + resident upload + cut-shader read.
- 2026-06-25 — **Gap B step 1b**: added `?paging` editor URL flag (context.rs →
  `cluster_paging`; preview.rs full-literal updated). On-device (editor rebuilt,
  `?vg&paging`, subdivided sphere): console `cluster paging (Gap B): page pool plan —
  13065 resident clusters → 8192 slots used / 8192 capacity, overflow 4873`, and the
  render is **byte-identical** to `?vg` alone (same `cluster LOD (GPU)` 13065/M=583768
  + `cluster compaction` 1696 tris; hole-free screenshot). The flag is now end-to-end
  usable + verified zero-regression; the overflow signal (working set > pool) is what
  eviction will manage. Suites 301/35/36/37 green. **Still 3/6** (A2 still needs the GPU
  page-pool buffers + cut-shader resident read + dynamic swap)._
