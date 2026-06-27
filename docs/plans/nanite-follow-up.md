# Nanite / cluster-LOD follow-ups — hardening plan

Closing the last two open items from [`../nanite-lod.md`](../nanite-lod.md) so the
nanite implementation is as robust as possible. The third historical follow-up
(editor cluster-asset persistence) is **already shipped** — `persistence::cluster_files`
+ `restore_cluster_meshes` round-trip every referenced DAG through
`assets/<source>.clusters.bin` in all save/load paths; do not re-do it.

Two SSOT references for the architecture: the runtime cut/paging lives in
`packages/crates/renderer/src/render_passes/cluster_lod/`, the bake in
`packages/crates/lod-bake/`, the editor glue in
`packages/frontend/editor/src/engine/bridge/` + `controller/`.

> **Working rule:** flags-off must stay byte-identical (the `lod` /
> `virtual_geometry` / `cluster_paging` Cargo features and runtime flags gate ALL
> of this). Every phase ends green on `cargo check -p <crate> --target
> wasm32-unknown-unknown` (editor) / native `cargo test` (lod-bake), and the
> renderer's existing nanite tests must still pass.

---

## Follow-up A — multiple simultaneous nanite meshes

### Current state (verified by recon, 2026-06-27)
The architecture is **already multi-mesh-shaped** — the "one resident mesh" line in
the old doc is largely stale, but the path is **untested** and has two genuine gaps:

- `ClusterLodRenderPass.states: Vec<ClusterMeshState>` keyed by `MeshKey`, find-or-create
  in `upload_pages`, and `dispatch_all` already `for state in &self.states` —
  `render_pass.rs:36, 206, 435, 509`. Per-mesh buffers/bind-groups/draw-args/paging
  all live on `ClusterMeshState` (`buffers.rs`).
- Per-draw override looks up state by `mesh_key` (`meshes/mesh.rs:299`).
- Editor cache is `HashMap<AssetId, Rc<ClusterMesh>>` (`bridge/cluster_cache.rs:27`);
  each `NodeKind::ClusterMesh` node materializes independently
  (`bridge/node_sync.rs:1050`, unique `label` per source).
- `persistence::cluster_files` already loops `cluster_sources(ctrl)` → every referenced
  DAG persists, multi included.

### Genuine gaps to close
1. **Untested.** No test imports/renders two cluster meshes at once. Unknown whether
   it actually works on-device.
2. **Diagnostic readback is first-mesh-only** — `dispatch_all` returns the readback for
   `states.iter().find(|s| s.cluster_count > 0)` (`render_pass.rs:~566`). Cosmetic, but
   misreports total resident tris with >1 mesh.
3. **Residency budget is per-mesh, not global.** `CLUSTER_STREAMING_BUDGET_TRIS = 1_000_000`
   is applied per cluster mesh in `scene-loader/src/lib.rs:1872`, and paging caps
   `MAX_LOADS = 96`/frame per state (`render_pass.rs:279`). With N meshes, VRAM and
   per-frame stream cost scale ×N **unbounded** — the real robustness hole. A scene
   with several heavy nanite meshes can blow the GPU pool (cf.
   [[oversized-gpu-buffer-guard]] — the 1.9 GB create_buffer cap will now *return Err*
   rather than abort, so the failure mode is a missing mesh + error log, not a crash —
   but that's degraded, not robust).

### Plan
- **A0 — prove the baseline.** Add a multi-mesh test (renderer-level integration test
  alongside the existing cluster tests; or scene-loader test) that materializes two
  distinct cluster DAGs and asserts both produce non-zero `draw_args.index_count` and
  distinct `MeshKey` states. This tells us what (if anything) actually breaks before we
  touch budgets. If it already passes, A1 collapses to docs + the budget work.
- **A1 — fix the readback** to aggregate across all resident states (or return a
  per-mesh vec), so diagnostics + the benchmark harness report correct totals with
  >1 mesh. Keep the single-mesh path byte-identical.
- **A2 — global residency budget.** Introduce a shared budget across all resident
  cluster meshes instead of a per-mesh constant: a `ClusterResidencyBudget` (total tris
  *and* total bytes) owned by the render pass / renderer, divided across resident states
  (proportional to each mesh's full-DAG size, or simple even split as a first cut).
  Wire the existing `?streambudget=N` knob to the *global* cap. Per-frame `MAX_LOADS`
  becomes a global stream budget shared round-robin across states so one mesh can't
  starve another. Document the policy.
- **A3 — editor multi-import.** Confirm (test or scripted MCP) that importing a 2nd
  `.clusters.bin` keeps the 1st resident, both render, both survive Save→reload
  (`cluster_files` already supports it — add an editor-level test asserting two
  `cluster_sources`). Verify the per-node transform is honored for each (each rides a
  child of its node's transform — `node_sync.rs:1048`).
- **A4 — verify on-device** in the model-viewer / editor: two heavy nanite meshes
  visible simultaneously, cut scales with viewport, no pool overflow under the global
  budget. Use the [[renderer-tracing-in-browser-console]] readback method.

### Acceptance
- Two+ nanite meshes render simultaneously, each with its own correct cut.
- Total resident VRAM is bounded by a single global budget regardless of mesh count
  (proven by a test that loads N meshes and asserts summed resident tris ≤ budget).
- Diagnostics/benchmark report correct totals.
- Save→reload restores all cluster meshes.
- Flags-off byte-identical; existing nanite tests green.

---

## Follow-up B — degenerate / pathological topology robustness

### Current state (verified by recon)
- **The bake's crack-free guarantee** depends on: weld-for-adjacency
  (`DagOptions::weld_eps`, `dag.rs:106`), identical boundary locking between adjacent
  groups (`simplify.rs:40,230` — `lock_boundaries` promotes shared boundary verts to
  Corner), and group-shared LOD bounds spheres so siblings flip together
  (`dag.rs:206`). The A1 test `non_watertight_sphere_cut_is_closed_at_every_level`
  (`dag.rs:462`) pins this for the common split-vertex case.
- **The CLI has a degeneracy guard** (`tools/lod-bake-cli/src/main.rs:178`): if
  `avg_tris_per_cluster < 8` OR `dag_ratio (dag_tris/source_tris) > 6`, it **skips**
  writing `.clusters.bin` (discrete LOD still emitted) unless
  `--allow-degenerate-clusters`. Pathological topology (non-manifold edges used by >2
  tris, unweldable) defeats clustering → ~1 tri/cluster and a ballooning DAG.
- **The editor bake has NO such guard** — `controller/lod_bake.rs:47 bake_static_clusters`
  only checks `CLUSTER_MIN_TRIANGLES`, then serializes the DAG unconditionally. So an
  in-editor export / nanite import can ship a degenerate, hole-prone DAG silently. This
  is the primary robustness hole.
- The heuristic is **duplicated** (CLI-only, hand-inlined) rather than shared in the
  `lod-bake` crate both call sites use.

### Plan
- **B1 — lift the guard into `lod-bake` (single source of truth).** Add a
  `ClusterMesh::quality(source_tris) -> DagQuality { avg_tris_per_cluster, dag_ratio,
  is_degenerate }` (or a free fn `cluster_dag_quality(&ClusterMesh, source_tris)`) in
  `lod-bake`, with the heuristic + thresholds as named consts. Unit-test it on healthy
  vs degenerate fixtures.
- **B2 — apply it in the editor bake (guard parity).** `bake_static_clusters` calls the
  shared quality check; on degenerate, **skip the cluster DAG and fall back to the
  discrete LOD chain** (already baked by `bake_static_lod`), with a `tracing::warn!`
  naming the asset + the metrics. Mirror the CLI's escape hatch as a build/runtime
  toggle if one is warranted (default: never ship a degenerate DAG). Refactor the CLI to
  call the same shared fn so the two can't drift.
- **B3 — strengthen the bake on non-manifold input.** Investigate edges incident to >2
  triangles (`cluster.rs:91 edge_tris` already tolerates them) — ensure grouping +
  boundary classification don't tear at non-manifold edges. At minimum, classify
  non-manifold edges as locked boundaries so they never get simplified asymmetrically.
  Add a fixture + test.
- **B4 — tests.** Add a non-manifold / unweldable fixture (e.g. a T-junction or a fan
  with a >2-incidence edge, plus a fully-split mesh that *can't* be welded by eps).
  Assert: (a) the quality fn flags it; (b) the editor bake emits discrete LOD and no
  `.clusters.bin`; (c) for any DAG that IS emitted, extend the A1-style crack-free check
  (`boundary_edge_count == 0` at every level) to the new fixtures that are valid.
- **B5 — runtime backstop (optional, low cost).** On cluster-mesh load
  (`scene-loader::materialize_cluster_mesh`), a cheap sanity check (e.g. clusters
  non-empty, indices in range, avg tris/cluster sane) that logs + refuses to materialize
  a malformed DAG rather than rendering holes. Prevention at bake is primary; this is
  defense-in-depth for hand-authored / third-party `.clusters.bin`.

### Acceptance
- One shared degeneracy heuristic in `lod-bake`, used by BOTH the CLI and the editor
  bake (no duplication).
- The editor never silently ships a degenerate cluster DAG — it falls back to discrete
  LOD and warns.
- Non-manifold edges are locked, not torn; new crack-free tests pass.
- Existing tests (A1, weld, monotonicity, messy-input) stay green.
- Flags-off byte-identical.

---

## Sequencing & verification

1. **B first** (self-contained, native-testable, highest robustness payoff): B1 → B2 →
   B3 → B4 → B5.
2. **Then A**: A0 → A1 → A2 → A3 → A4.
3. Update [`../nanite-lod.md`](../nanite-lod.md) status section as each follow-up closes
   (remove from "Known follow-ups", add to shipped/verified with the new test names).

**Per-phase gate:** `cargo test -p awsm-renderer-lod-bake` (B), `cargo check -p
awsm-renderer-editor --target wasm32-unknown-unknown` (editor edits), the renderer's
nanite test suite, and — for A4/B-runtime — an on-device check via chrome-devtools MCP
reading the cut readback from the browser console (see [[renderer-tracing-in-browser-console]],
[[aa-verify-in-model-viewer]]). The baked sample at `~/Downloads/baked` (a 60-node FBX,
27 nodes with `.clusters.bin`) is a ready multi-mesh + varied-topology fixture for A4.

## Status checklist
- [x] B1 shared degeneracy heuristic in `lod-bake` + unit test — `ClusterMesh::quality` +
  `DagQuality` + `MIN_AVG_TRIS_PER_CLUSTER`/`MAX_DAG_TRI_RATIO` consts
  (`cluster_mesh.rs`); tests `quality_passes_healthy_dag`,
  `quality_flags_degenerate_unwelded_split_mesh`.
- [x] B2 editor bake guard parity + discrete-LOD fallback; CLI refactored to share —
  `bake_static_clusters` now calls `cm.quality(tris)` and returns empty (discrete LOD
  still ships) on degenerate, with a `tracing::warn!`; CLI `bake_one` uses the same
  `quality()` instead of its inlined heuristic. No editor escape hatch (an authoring
  tool must never silently ship a cracking nanite mesh).
- [x] B3 non-manifold edge locking + fixture — `nonmanifold_locked` in `simplify.rs`
  marks endpoints of every ≥3-incidence edge; they're forced to `VertClass::Corner`
  before collapse so disjoint sheets can't fold together / tear (and the cut can't
  crack where they meet). Manifold meshes (all edges 1/2) → no-op. Tests:
  `nonmanifold_locked_flags_only_high_valence_edges`,
  `nonmanifold_yprism_simplifies_without_tearing`.
- [x] B4 degenerate/non-manifold tests + extended crack-free coverage — `dag.rs`:
  `non_watertight_torus_cut_is_closed_at_every_level` (A1 extended to a genus-1
  surface), `triangle_soup_is_flagged_degenerate` (unweldable soup flagged under
  DEFAULT welding — the real editor-bake fallback trigger). Plan item (b) "editor
  emits discrete LOD, no .clusters.bin" is enforced by the shared B2 guard and
  covered by the player-bundle export self-verify; the editor crate has no native
  test target (per `lod_bake.rs`), so it's not a separate unit test.
- [x] B5 runtime load-time DAG sanity backstop — `ClusterMesh::validate()` (lod-bake,
  native-tested) checks every page span is in range + triangle-aligned and every
  index references a real vertex; `materialize_cluster_mesh` calls it after the empty
  check and refuses to materialize a malformed DAG (logs + renders nothing instead of
  holes / GPU OOB). Verified by `validate_accepts_real_bake_and_rejects_corruption`
  (native) + the wasm build of the call site. On-device verification N/A — the guard
  only fires on deliberately-corrupt input; a fabricated bad file on-device would add
  nothing over the native test.
- [x] A0 two-mesh baseline — ALREADY MULTI-MESH (confirmed by code, no change needed):
  `upload_pages` find-or-creates a `ClusterMeshState` per `render_mesh` MeshKey, each
  with its own buffers/bind-groups (`render_pass.rs:435`); `dispatch_all` loops every
  state (`:509`); the per-draw override resolves state by `mesh_key` (`meshes/mesh.rs:299`);
  nothing clears states or asserts a single one. Each editor `ClusterMesh` node
  materializes with a fresh MeshKey ⇒ N nodes → N states. No native GPU test harness
  exists in the renderer (only shader-template unit tests), so the live two-mesh render
  proof is A4. Real remaining gaps: A1 (readback) + A2 (global budget).
- [x] A1 multi-mesh-correct diagnostic readback — `dispatch_all` now returns one
  `(readback_buffer, cluster_count)` per resident mesh (each state already owns a
  readback buffer); the render.rs consumer maps them all, sums drawn index counts,
  and logs `drawn index_count = N (T tris) across M mesh(es) over C clusters`. Gated
  by `lod`; flags-off compiles unchanged. Verified: renderer wasm (lod on + lod off),
  editor wasm, lod-bake 42 tests.
- [x] A2 global residency budget shared across meshes — `ClusterMeshState.resident_tris`
  + `ClusterLodRenderPass::resident_tris_total()` + `AwsmRenderer::cluster_resident_tris_total()`
  expose total residency; `materialize_cluster_mesh` caps a new mesh's budget at
  `min(per_mesh, global_cap − already_resident)` where `global_cap =
  per_mesh_budget * GLOBAL_RESIDENCY_MESH_MULTIPLE (8)`, and skips (renders nothing +
  warns) when 0 left. Bounds total VRAM at ~8× per-mesh regardless of mesh count.
  DESIGN NOTES / deviations from the plan sketch: (1) bounds by triangles (the GPU
  pool's dominant cost is the exploded vertex buffer at 56 B/index, ∝ tris) — a
  separate byte cap would duplicate the existing per-buffer 1.9 GB guard, so not added.
  (2) Kept `?streambudget` as the per-mesh budget (not repurposed as the global cap) so
  existing behavior is preserved; the global cap scales with it (8×). (3) The caps-off
  path (budget == MAX, streaming+paging both off) stays uncapped ⇒ shipped path
  byte-identical. (4) First-cut policy is first-come (earlier meshes keep full detail,
  later ones throttle then drop) — fair re-budgeting on load is future work. Verified:
  renderer wasm (lod on + off), editor wasm, lod-bake 42 tests. Live bounded-VRAM
  proof: A4.
- [x] A3 editor multi-import + Save→reload test — native test
  `cluster_sources_from_project_collects_every_mesh` (persistence.rs) asserts a project
  with several `ClusterMesh` nodes (incl. nested) yields every distinct source — the
  exact set `cluster_files` writes and `restore_cluster_meshes` re-reads, so multiple
  nanite meshes survive Save→reload. (Editor bin DOES run host tests — 28 pass — the old
  "no native test target" note was stale.) Multi-import keeps prior nodes (the
  `ImportNaniteAsset` handler inserts without clearing) and each node materializes under
  its own `sub_tk` (per-node transform), both confirmed by code; live render + reload
  proof folds into A4.
- [ ] A4 on-device verification (two heavy meshes, bounded VRAM)
- [ ] docs: `nanite-lod.md` status updated as each closes
