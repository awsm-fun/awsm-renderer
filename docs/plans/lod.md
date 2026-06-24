# Level-of-Detail (LOD) for AwsmRenderer

> Roadmap / SSOT for built-in LOD. Supersedes the old `nanite.md` (deleted).
> The high-risk software-rasterizer + streaming work is split out into
> [`nanite-software-rasterize.md`](nanite-software-rasterize.md) — a separate,
> test-gated future bet that this plan does **not** depend on.

## Goal

Built-in LOD is essential for real-world game playing. We want it to be:

1. **General** — wins for all sorts of games, not one genre.
2. **Player-first** — runtime/player performance is what matters; editor LOD
   preview is a secondary concern.
3. **Material-agnostic** — LOD is a property of *geometry*; the material model
   is untouched. Geometry resolves into the same visibility buffer either way;
   the material passes only change *where vertex indices come from*.

Backwards compatibility is a non-issue (not public yet), so we are free to make
LOD a first-class part of the asset/bake format.

## The core split: LOD by mesh class

There is no single LOD technique that covers every mesh, because **cluster LOD
structurally cannot represent deforming geometry**. Clusters are baked in
object space with fixed topology, per-cluster bounds, monotonic error, and
boundary-locked seams — skinning and morph move vertices arbitrarily per frame
and invalidate all of that. This is intrinsic, not an asset-pipeline
limitation. So LOD routes by class:

| Mesh class | LOD technique | Why |
|---|---|---|
| **skinned / morph** (deforming) | **Discrete LOD chain** | Cluster LOD can't represent it. Each level is a normal skinned mesh with fewer verts and preserved skin weights. Covers crowds of characters — one of the most LOD-sensitive cases in real games. |
| **static rigid** (no per-vertex deform; per-object translate/rotate/scale fine) | **Cluster LOD DAG** (Nanite-style, HW raster) | Crack-free continuous LOD; decouples draw cost from object/mesh count via one compacted stream. Where the GPU-driven visibility-buffer architecture pays off. |

These are **complementary, not redundant** — discrete LOD is not deprecated by
cluster LOD; it is the half of the matrix cluster LOD can never reach. The
existing classifier (`node_is_skinned` in the editor; `RawMeshData.skin` /
`RawMeshData.morph` at the API level) already distinguishes the two.

**One bake tool, two outputs, routed by class.** The shared piece is the
simplifier. Discrete = run it N times with weight preservation. Cluster =
graph-partition + boundary-locked simplify + error-monotonic regroup into a DAG.
The runtime paths differ (see Phase A / B).

**Simplifier implementation — pure Rust, not the meshopt C lib (forced by the
wasm build).** The bake runs inside `bake_player_bundle`, which lives in the
**editor frontend** — a `wasm32-unknown-unknown` crate. The `meshopt` crate
compiles vendored meshoptimizer C++ via `cc`, and the toolchain here (Apple
clang) has no `wasm32` target, so it cannot build for the editor. The shared
simplifier is therefore a pure-Rust **boundary-locked half-edge QEM collapse**
(`awsm-renderer-lod-bake`): it collapses each edge onto one of its two *existing*
endpoints (never synthesizing new vertex positions), so the surviving vertices
are always a **subset** of the originals. That subset property is what makes
skin-weight / morph-target carry-through *exact* — a level's vertices keep their
original JOINTS/WEIGHTS and morph deltas verbatim, no interpolation. Boundary +
attribute-seam vertices are locked so silhouettes/seams stay stable across
levels. (This realizes the plan's `meshopt_simplify` intent; the specific C call
is unavailable in-target.)

## Where the bake runs: the build boundary, not import

The editor lifecycle has three moments:

1. **Import** — glb is *decomposed* (clips + materials stripped, geometry lifted
   into a `CapturedMesh` in `mesh_cache`). This is normalization, not
   optimization.
2. **Save** — `CapturedMesh` → `assets/{id}.mesh.bin` (editable authoring SSOT).
3. **Player-bundle export** (`bake_player_bundle`) — the build boundary.

**LOD-bake runs at export, on resolved final geometry — never at import.**
Reasons:

- Geometry stays editable after import (a mesh carries `editable: bool`, a
  `ModifierStack`, and sparse `VertexOverrides`). An import-time bake goes stale
  the moment any of those change. Geometry is only *final* at the build
  boundary.
- LOD is a delivery optimization (players > editor), so it belongs in the build
  artifact, not the editable source — mirroring how the existing 56-byte
  visibility packing (`pack_visibility_bytes`) is already a build/load-time
  derived artifact that is never persisted in the project.
- Keeps the editable project a clean source-of-truth.

**Content-hash caching:** key the baked output on `hash(resolved geometry + LOD
settings)` so unchanged meshes don't re-bake on every export. Recovers
import-time-like speed without the staleness.

**Resolved geometry is already available at export — no extra step needed
(verified against the codebase):**

- `mesh_cache::get_raw(id)` returns **fully-resolved final geometry**: the cache
  (`CapturedMesh` / `.mesh.bin`) is kept continuously in sync — every modifier or
  vertex-override edit re-runs `evaluate_def` (stack → overrides → baked
  `MeshData`) and re-stores the cache (`controller/mesh_eval.rs`,
  `ensure_authorable` in `controller/state.rs`).
- `bake_player_bundle` already writes `get_raw(id)` → glb, i.e. final vertices
  with the modifier stack and overrides fused in. For **skinned/morph** meshes the
  source is instead `skinned_bake_cache::get_rig_glb(src)` — the clean re-exported
  rig glb (skeleton + mesh + skin weights + morph targets), built at import.
- The **player runtime has no modifier/override concept** — `RuntimeMesh` is
  `Primitive | Glb` only; modifiers/overrides are editor-only. So the exported
  geometry is authoritative; nothing re-applies later.
- `collapse_mesh_stack` is an optional, destructive *user* action and is **not**
  required for export.

Therefore the LOD bake operates directly on `get_raw(id)` (static) /
`get_rig_glb(src)` (skinned). There is **no open design question** here — the bake
input is final geometry by construction.

## Per-mesh LOD toggle

LOD is opt-**out**, per mesh, default ON.

- **Default ON** — LOD is the norm for a general game renderer.
- **Opt-out cases** — hero assets where any simplification is unacceptable,
  already-low-poly meshes (bake cost, no benefit), HUD/UI meshes.
- **Home**: a `MeshLodConfig` sibling to the existing `MeshShadowConfig` on the
  mesh node. Persists in `project.toml` automatically (same as shadow
  cast/receive). **Authored in the editable project, consumed by the export
  bake** — it makes no sense at import, matching the build-boundary decision
  above.
- **Reachable via both UI and MCP** (required, not optional):
  - **UI** — a control in the mesh inspector next to the existing shadow
    cast/receive toggles.
  - **MCP** — a `set_mesh_lod` tool mirroring the existing `set_mesh_shadow`
    (same dispatch path, snapshot exposes the current value). Anything the UI can
    set, MCP can set, and vice-versa.
- **Class-agnostic at the toggle**: one `enabled: bool` to start. The bake
  routes by the existing skinned/static classification. Grow later to carry
  params (target ratios, level count, error threshold).
- Pair with a **global default** (LOD on) + per-mesh override.

**Status — landed (A.1):** `MeshLodConfig { enabled: bool }` (default on) is a
sibling of `MeshShadowConfig` on the `Mesh` / `SkinnedMesh` variants and
`InstancesAlongCurveDef` (`scene/src/tree.rs`, `scene/src/instances.rs`), with
`NodeKind::mesh_lod()` / `mesh_lod_mut()` accessors. It persists in
`project.toml` automatically (serde, `#[serde(default)]` ⇒ legacy projects load
as enabled — round-trip test in `tree.rs`). Reachable from the editor inspector
("LOD" section, `mesh_lod_editor` in `scene_mode/inspector.rs`) and the
`set_mesh_lod` MCP tool (mirrors `set_mesh_shadow`, `mcp/src/mcp.rs`). Consumed
by the export bake (A.2), not at import/runtime.

---

## Phase A — Discrete LOD chain (ship first)

The 80/20. Rides the **existing** per-mesh GPU-driven pipeline almost unchanged
and covers skinned/morph meshes that cluster LOD can never handle.

**Bake (export-time):**
- For each LOD-enabled mesh, generate N progressively simplified levels with
  `meshopt_simplify`, boundary-locked so silhouettes/seams stay stable across
  levels. Bake input by class:
  - **static rigid** → simplify `get_raw(id)` (positions/normals/uvs/colors/
    indices).
  - **skinned/morph** → simplify the rig glb's mesh (`get_rig_glb(src)`) and
    **carry skin weights through simplification** (remap JOINTS/WEIGHTS to the
    surviving vertices; use attribute-aware simplification so weights aren't
    discarded) and **carry morph targets** (simplify each target's deltas against
    the same surviving-vertex map so blend shapes still line up). This is the
    extra work that makes discrete LOD valid for deforming meshes — get it right.
- Emit each level as additional geometry in the player bundle, plus a small
  per-mesh LOD descriptor (level count + screen-error/distance thresholds +
  bounds).
- Skip the bake when the mesh's `MeshLodConfig.enabled == false`, or when it is
  already below a min-triangle threshold (no benefit).

**Runtime:**
- Each discrete level is just another `MeshKey`. The existing occlusion-cull +
  compaction pass already does per-instance visibility selection — extend it to
  pick *which level's* `MeshKey` to bump per instance, by projected screen-space
  error (reuse the screen-AABB math already in `cull.wgsl`).
- No new raster path, no vis-buffer change, no material change. Material-agnostic
  by construction — a LOD swap is pure geometry.

**Cost / trade-off:** popping at transitions (acceptable for the discrete tier;
this is the well-understood classic technique). Authoring/bake of the levels is
automated.

**Status — landed (A.2 core):** the `awsm-renderer-lod-bake` crate exists with
the shared simplifier: `simplify(positions, indices, opts) -> SimplifiedMesh`
and `build_lod_chain(positions, indices, ratios)`. Pure-Rust boundary-locked
half-edge QEM collapse (see "Simplifier implementation" above); builds for
`wasm32-unknown-unknown`; unit-tested (flat plane → lossless, boundary verts
survive, curved surface → nonzero error, attribute gather, monotone chain).
`SimplifiedMesh { surviving, indices, error }` + `gather<T>(attr)` give the
caller the subset remap to carry positions/normals/uvs/colors/skin/morph through
a level.

**Status — landed (A.2b, static wiring):** `bake_player_bundle` now bakes the
discrete chain for LOD-enabled, above-floor (≥512 tri) **static** Glb meshes.
Format (additive — no `scene.toml`/`RuntimeMesh` change, so flag-off bundles are
byte-identical in everything the renderer reads): per mesh asset `<id>` it emits
`<id>.lod{N}.glb` per simplified level + an `<id>.lod.toml` manifest
(`MeshLodManifest`: bounds radius, base tri count, per-level index/error/tris).
The level-planning policy (floor, drop non-reducing levels, numbering, manifest)
lives in `lod-bake::plan` (native-tested); the editor side
(`controller/lod_bake.rs`) is only attribute-gather + glb-encode + filename +
a geometry-hash session cache. Per-node toggle governs per-asset bake (an asset
bakes if any referencing node is LOD-enabled). End-to-end verified via MCP
export on DamagedHelmet (15452 → 10074 tris, manifest error 0.164, no panic).

**Known follow-up before the acceptance test — simplifier aggressiveness.** The
current rule *hard-locks every boundary/seam vertex*, which on seam-heavy meshes
(e.g. DamagedHelmet) locks so much that the chain plateaus (~65% of base, only
one level survives) instead of reaching the requested 0.5/0.25/0.125. Fix:
allow a boundary/seam vertex to **slide along its boundary** (collapse onto
another boundary vertex sharing a boundary edge) instead of locking it outright
— standard boundary-preserving QEM. Discrete LOD swaps whole meshes, so this
doesn't risk cross-mesh cracks. Do this next, then A.2c (skinned/morph) + A.3
(runtime selection).

**Critical files:**
- Runtime selection: `render_passes/occlusion/shader/occlusion_wgsl/cull.wgsl`,
  `render_passes/occlusion/compaction.rs`
- Mesh ingestion: `src/mesh_pack.rs`, `src/raw_mesh.rs`, scene-loader
- Bake: new `awsm-renderer-lod-bake` crate (shared simplifier; see Phase B),
  export hook in `editor/src/controller/export.rs` (`bake_player_bundle`)
- Per-mesh toggle: `MeshLodConfig` on the mesh node (parallel to
  `MeshShadowConfig`); plumb through `editor-protocol/src/mesh_def.rs`, the
  editor inspector UI, and a `set_mesh_lod` MCP tool (mirror `set_mesh_shadow`)
- Skinned source: `skinned_bake_cache::get_rig_glb`; static source:
  `mesh_cache::get_raw`

---

## Phase B — Cluster LOD DAG, HW raster (the continuous-LOD answer)

For static rigid meshes. Crack-free continuous LOD; collapses many distinct
meshes into one compacted draw, decoupling cost from object count. This is the
real architectural investment and where the visibility-buffer head-start pays
off. **HW-raster only** — no software rasterizer, no streaming (those live in
[`nanite-software-rasterize.md`](nanite-software-rasterize.md) and are not
required for this phase to deliver).

The renderer is already a GPU-driven visibility-buffer deferred renderer, so the
backbone is reused, not rebuilt:

- **Vis buffer** — `render_passes/geometry/shader/geometry_wgsl/fragment.wgsl`
  already writes `triangle_index + material_mesh_meta_offset`. Add `cluster_id`
  to the payload (re-budget the bits).
- **GPU cull** — frustum + Hi-Z in compute (`render_passes/occlusion/`,
  `render_passes/hzb/`) generalizes from per-mesh to per-cluster.
- **Compaction** — `compaction.rs` today emits one `drawIndexedIndirect` slot
  per `MeshKey`. WebGPU has no `multiDrawIndirect`, so cluster compaction builds
  **one** compacted index stream → a single indirect draw.
- **Deferred resolve** — `material_prep/` + `material_opaque/` re-point their
  triangle-vertex fetch from the per-mesh index pool to **cluster index pages**
  (`cluster_id` → page → 3 indices → bary interpolation). The shading model is
  unchanged — material-agnostic.

**B.1 — Offline cluster bake** (extends the Phase A bake tool):
- Cluster generation (~128 tris/cluster) via `meshopt_buildMeshlets`.
- LOD DAG: group adjacent clusters (graph partition, e.g. `metis`), simplify
  each group with **locked shared boundaries** (boundary-lock = crack-free,
  non-negotiable), re-split into coarser clusters, record per-group monotonic
  error + bounding sphere.
- Emit cluster vertex pages, index pages, per-cluster meta (local bounds,
  parent/child links, LOD error, material id). Retain the 56-byte exploded
  visibility vertex layout per-cluster.

**B.2 — Cluster cull + LOD selection (compute):**
- Two-level cull: cheap per-instance frustum/HZB over instance bounds
  (generalizes today's `OcclusionInstance` array), then per-cluster LOD cut only
  inside survivors.
- LOD cut: per cluster group, compare projected screen-space error vs threshold
  to choose parent-vs-children. Projection uses the instance world transform
  incl. scale. Non-uniform scale/skew needs conservative bounds (AABB/OBB) +
  error scaled by max axis.
- Compaction emits the visible-cluster list + one packed index buffer for a
  single `drawIndexedIndirect`.

**B.3 — Vis-buffer payload + material integration:**
- Re-budget `visibility_data` to carry `cluster_id` + triangle-in-cluster +
  material routing. Update `split16`/`join32` usage in `fragment.wgsl` and all
  readers.
- Re-point attribute reconstruction in `material_prep/.../compute.wgsl` and
  `material_opaque/.../compute.wgsl` at cluster index pages.
- Respect the prep-vs-recompute standard (`docs/SHADER_GUIDELINES.md`) and the
  MSAA-compile invariant — edges are now cluster-scale; flag as a
  standards-review item.

**Coexistence:** cluster and non-cluster (incl. all skinned/morph + discrete-LOD)
geometry converge on the **same** visibility buffer, so `material_prep` /
`material_opaque` keep working for both.

**Critical files** (in addition to Phase A's):
- Vis-buffer write: `render_passes/geometry/shader/geometry_wgsl/{vertex,fragment}.wgsl`, `geometry/pipeline.rs`
- Cull/LOD/indirect: `render_passes/occlusion/{cull.wgsl,compaction.rs,buffers.rs}`, `render_passes/hzb/`
- Material resolve: `render_passes/material_prep/.../compute.wgsl`, `render_passes/material_opaque/.../compute.wgsl`
- Bake crate: `awsm-renderer-lod-bake` (depends `meshopt`, `metis`)
- Scheduling/features: `src/render.rs`, `src/features.rs`

---

## Cross-cutting

- **Feature gate**: add `lod` (discrete, Phase A) and `virtual_geometry`
  (cluster, Phase B) to `features.rs`, default off initially, mirroring
  `gpu_culling`. With the flag off, assert byte-identical output to today
  (default-must-equal-today). Direction is default-on once proven.
- **Sequencing**: no external dependency (the `awsm-renderer-*` crate rename
  shipped at 0.4.0). Built on the `lod-nanite` branch. Within this plan, Phase A
  (discrete) ships first, then Phase B (cluster).
- **No per-frame heap allocs** in the hot path (David's standard) — pool/avoid
  in the runtime selection + cull paths. Verify with `?stress=N` +
  `?trace=sub-frame`.

## Verification

**Per-phase (during development):**
- Bake reference assets (a skinned character for Phase A; a multi-million-tri
  static mesh for Phase B). Load with `task model-tests:dev` (port 9080) and the
  editor (`task editor-dev`, port 9085).
- chrome-devtools MCP for perf traces (frame time, triangle throughput) and
  screenshots.
- Phase A: confirm correct level selection while dollying; measure draw-call /
  triangle reduction for crowds.
- Phase B: cross-check the vis buffer via the existing GPU picker compute path;
  confirm crack-free LOD transitions while dollying (boundary-lock validation).
- Toggle parity: set `MeshLodConfig` via **both** the editor UI and the
  `set_mesh_lod` MCP tool; confirm the snapshot + exported bundle agree.
- Gate hygiene: feature off ⇒ byte-identical to today.
- `cargo test -p awsm-renderer -p awsm-renderer-materials -p awsm-renderer-scene-loader --lib`
  before each commit.

**End-state acceptance test (mandatory once everything lands).** Build **one
mixed scene** exercising the full matrix and verify it via chrome-devtools MCP
(screenshots + perf trace at near/mid/far camera distances):

- Geometry classes present together in the same scene:
  - **static rigid** (cluster-LOD path),
  - **skinned** (discrete-chain path, animating),
  - **morph** (discrete-chain path, blend shape driven).
- For **each** class, include instances with the **LOD toggle ON and OFF**
  (the full on/off × class matrix — 6 combinations minimum), so every routing
  branch is hit in a single frame.
- Assert:
  1. **Correctness** — toggle-OFF instances render at full detail; toggle-ON
     instances select the expected level by distance; no cracks/popping beyond
     the discrete tier's known popping; skinned/morph instances still deform
     correctly at every level (weights + morph targets survived simplification).
  2. **Coexistence** — cluster and discrete geometry share one visibility buffer
     and shade identically to a non-LOD reference (material-agnostic check).
  3. **Perf** — measurable frame-time / draw-call / triangle-throughput win at
     mid/far distance vs. the all-toggle-OFF baseline of the same scene.
  4. **No per-frame heap allocs** in the hot path under `?stress=N` +
     `?trace=sub-frame`.
- The autonomous loop must run this matrix explicitly and report the
  before/after numbers — not just "it renders."
