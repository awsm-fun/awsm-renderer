# Mesh-editing + GLB-export — STATUS

Running log of the mesh-editing/GLB-export arc (spec: `docs/plans/mesh-editing.md`).
Branch: `mesh-authoring`. Native gates used each commit: `task lint` (fmt + clippy
`-D warnings`, whole workspace incl. tests) + relevant `cargo test`.

Legend: ✅ done & gated · 🟡 implemented, needs in-browser verification · ⬜ not started.

## Group B — LIVE-VERIFIED via MCP (driven against the running editor)
All confirmed end-to-end through `task mcp-dev` + a Chrome WebGPU tab:
- **Phase 1 export:** `export_node_glb` on a box → valid GLB (POSITION min/max,
  PBR material). Custom-WGSL material → `export_node_glb` shows
  `extensionsUsed:[AWSM_materials_none]`, **no** embedded material, primitive
  `extensions.AWSM_materials_none.id` = the material id. ✅ (import recognition is
  in renderer-gltf.)
- **Phase 2:** `convert_to_editable_mesh` → node becomes `Mesh`, renders, material
  edits apply (after the patch_builtin_param fix). ✅
- **Phase 3:** `set_mesh_modifiers` subdivide+twist → twisted prism + undo
  restores; lathe bat profile → `get_mesh_cross_section` reads the radii. ✅
- **Phase 4:** `select_vertices_where` (top_percent → 1470 rim verts);
  `soft_transform_vertices` flared the rim (bbox 0.6→0.996, watertight) → undo
  exact; `collapse_mesh_stack` preserves geometry. ✅
- **Phase 5:** `set_mesh_modifiers` SDF mug graph → 22k-tri watertight hollow cup
  with handle (screenshot + stats). ✅
- **Phase 6:** `export_player_bundle` → scene.glb with `KHR_lights_punctual`,
  pruned `materials/*.wgsl`+`.toml`, `AWSM_materials_none` wiring, env descriptor;
  `export_scene_glb` carries a rotation **animation** (LINEAR sampler, times,
  VEC4 outputs, right node). ✅
- **Introspection:** `get_mesh_stats` + `get_mesh_cross_section`. ✅
- **Fixes found+shipped while driving:** material params on Mesh/Sweep nodes;
  MCP string-encoded JSON args (`stack`/`predicate`/`query`).

## Finish line — COMPLETE (everything except the separate-repo player loader)

- **Texture embedding** ✅ implemented + live-verified: a procedural checker on a
  PBR material exports with `images:1` (PNG embedded in BIN), `textures:1`,
  `baseColorTexture` wired; no-texture material → `images:0` (referenced-only both
  ways). Export is async (procedural regen + raster GPU readback).
- **MCP robustness + discoverability** ✅ (the "guesswork" fix):
  - `awsm://docs/mesh-tools` resource — exact JSON shapes + copy-paste examples,
    served + verified.
  - **Strongly-typed tool params** via `schemars`: `set_mesh_modifiers.stack` =
    `ModifierStack`, `select_vertices_where.predicate` = `VertexPredicate` (full
    schemas published, incl. UUID-as-string). Wrapped in `Flexible<T>` so they're
    typed/self-documenting **and** tolerant of clients that send a nested object
    as a JSON string (the root cause of the earlier failures). `json_arg` retained
    only on the raw escape hatches (`dispatch_command`/`dispatch_batch`/`run_query`).
  - Two bugs found+fixed while driving: material params on Mesh/Sweep nodes;
    string-encoded args across all object tools.
- **Driven live (real concepts):** twisted/tapered column, baseball-bat lathe +
  cross-section, CSG mug (SDF), soft-transform spout + undo, superquadric pebble,
  formula-displaced **rock** with a live checker texture. All via MCP.
- **Out of scope (by decision):** the player-side bundle bundle loader lives in
  the separate game-player repo. One optional in-repo cosmetic remains: a
  read-only *vertex-selection highlight* in the viewport (the functional
  `select_vertices_where` query already works).

## Finish-line tracker (A → B → C)
- **Group A — pure code (done, all lint+native-gated):** Phase 6 animation
  lowering (TRS clips → glTF channels, writer natively tested); `AWSM_materials_none`
  import recognition in renderer-gltf; `SetMeshModifiers`/`SoftTransformVertices`
  per-mesh undo coalescing.
- **Group B — needs a live editor tab (next):** all browser/MCP checklists below
  + GLB texture-byte embedding (async ProjectDir reads) + transient
  `SetVertexSelection` highlight rendering. **Unblock:** `task mcp-dev`, open
  `http://localhost:9085/?mcp=http://127.0.0.1:9086` in Chrome.
- **Group C — separate repo:** the game-player bundle loader.

## Summary (where we are)
Phases **1–4** have their natively-testable cores **done + tested** and their
command/query/MCP surfaces **wired + lint-gated**, plus the LLM perceive→act
introspection loop. Native test counts: `glb-export` 6, `scene-schema` 13,
`meshgen` 28. Whole-workspace `task lint` green at every commit.

**Done (native-tested):** GLB writer + scene-complete IR; editable-mesh schema;
modifier-stack schema + full evaluator (incl. `Displace` formula evaluator);
mesh stats + cross-section; soft-transform + predicate selection.
**Wired (lint-gated, browser-pending):** ExportGlb; persistence side-files;
ConvertToEditableMesh / SetMeshData / SetMeshModifiers / SetVertexPositions /
SoftTransformVertices / CollapseMeshStack commands + mesh-revision bridge;
get_mesh_stats / get_mesh_cross_section / select_vertices_where queries; ~14 new
MCP tools.
**Next arcs:** Phase 6 player bundle (reuses the scene-complete `write_glb`; needs
a player-side bundle loader), the generated capabilities reference / mesh-edit
view, and all in-browser verification (checklists below).

Native test counts now: `glb-export` 6, `scene-schema` 14, `meshgen` 34.

---

## Phase 1 — GLB export

### ✅ Core crate `packages/crates/glb-export` (`awsm-glb-export`) — committed, native-tested
- Scene-complete IR: `GlbScene { nodes, animations, images, env }`,
  `ExportNode { name, transform, mesh, material, light, camera, children }`,
  `ExportMaterial::{Pbr,Unlit,None{id}}`, `ExportLight`, `ExportCamera`,
  `ExportAnimation`, `EnvRef`. Light/camera/animation/env slots exist now so the
  Phase-6 player-bundle writer reuses the IR without a rewrite.
- `write_glb(&GlbScene) -> Vec<u8>` via `gltf-json` (promoted to a workspace dep
  with features `names,extensions,KHR_lights_punctual,KHR_materials_unlit`) +
  hand-rolled GLB container. POSITION min/max emitted (reader validates it).
- Material policy: PBR → glTF PBR; Unlit → `KHR_materials_unlit`; non-PBR →
  `AWSM_materials_none` primitive extension + **no** embedded material. Textures
  referenced-only (`GlbScene.images` is the curated pool).
- `pub const AWSM_MATERIALS_NONE = "AWSM_materials_none"`.
- Tests (`cargo test -p awsm-glb-export`, 6 passing): cube round-trip re-parsed
  with the `gltf` reader (vertex/index counts + PBR factors); unlit + none wiring
  via raw JSON; referenced-only image embedding; light-only node (empty BIN).

### 🟡 Editor wiring — type-checked (`task lint` green), NOT browser-verified
- Protocol: `EditorQuery::ExportGlb { node: Option<NodeId> }` → base64 in
  `QueryResult::Text` (`packages/crates/editor-protocol/src/query.rs`).
- `packages/frontend/editor/src/controller/export.rs`: scene/subtree → `GlbScene`
  → `write_glb`. Covers **Primitive** (`node_sync::primitive_to_mesh`), **Mesh**
  (via `mesh_cache::get_raw`), **Sweep** (resolves the curve node from the scene
  tree + `sweep_along_curve`), **Light**, **Camera**, and the node hierarchy.
  Material mapping resolves custom-WGSL → `None{id}`, else assigned-library or
  inline `MaterialDef` → PBR/Unlit/None by `shading`.
- Query arm in `controller/state.rs`; MCP tools `export_scene_glb` /
  `export_node_glb` in `packages/mcp/src/mcp.rs`.

#### Morning checklist (browser / MCP)
1. `task editor-dev` (or the editor dev server), open the editor.
2. Insert a Box primitive, assign a plain PBR material, then via MCP:
   `export_node_glb { node: <id> }` → decode base64 → write `.glb` → open in a
   glTF viewer; confirm geometry + PBR factors. `export_scene_glb` for the tree.
3. Lightweighting: import a textured glb, reassign a no-texture PBR, export,
   confirm `images.len() == 0` (heavy textures dropped — no slim flag).
4. Custom-WGSL material → export → confirm primitive carries `AWSM_materials_none`
   and no embedded material.

#### ⬜ Phase 1 remaining (browser-only or deferred — do NOT block native gates)
- **Textures**: factors export; the image pool is empty because reading raster
  bytes off `ProjectDir` is async/browser-only. Implement: in `export.rs`, walk
  the assigned `MaterialDef`'s `*_texture: Option<TextureRef>`, load bytes via
  `ProjectDir` / the asset disk path (`asset_disk_path`), push `ExportImage`s,
  and set `TexRef`s. Needs the async controller + fs handle.
- **Model nodes**: re-read the source glb via `GltfLoader::load`
  (`engine/bridge/gltf.rs:148`) at export and pull POSITION/NORMAL/TEXCOORD/
  indices. **Risk (from spec): `ImportModelFromFile` blob: URLs are session-local
  and may be revoked** — mitigate by persisting imported source bytes into the
  project at import. Currently Model subtrees export as empty transform nodes.
- ✅ **UI button** (DONE + browser-verified): scene-level "Export scene as GLB…"
  in the overflow (⋯) menu (`app.rs::export_scene_glb`) + per-node "Export GLB"
  button in the inspector for geometry/Group/Model kinds
  (`inspector.rs::export_node_section`). Both trigger a binary **blob download**
  (`app.rs::download_bytes`) — re-import is the user's call. Verified live: both
  entry points downloaded a `.glb` that opened correctly in a third-party glTF
  viewer (torus).
- **Import round-trip `AWSM_materials_none`** (`renderer-gltf`): the importer
  today recognizes only the *singular*, material-level `AWSM_material_none`
  (`populate/material.rs:37` → maps to `UnlitMaterial`). The spec wants the
  *plural*, **primitive-level** `AWSM_materials_none` → leave the material slot
  empty for scene-level resolution. That recognition belongs in
  `populate/mesh.rs:~195` (it has the primitive; the material mapper only has the
  material). Reconcile the singular/plural tokens (and material- vs primitive-
  level) before implementing. Editor-side: the Model import (AssetTemplate → node)
  should map an empty slot to the scene's custom-material assignment using the
  id carried in the extension. **All browser-verified.**

---

## Phase 2 — Editable mesh asset + persistence — 🟡 implemented (lint + native-tested), browser-pending

### ✅ Schema (`scene-schema`) — native-tested
- `MeshDef.editable: bool` (`#[serde(default)]`); `CapturedSource::Editable` +
  `Imported { source }`. Tests: CapturedMesh bitcode round-trip, editable default
  false on old JSON, both new variants through serde + bitcode.

### 🟡 Persistence gap (`controller/persistence.rs` + `mesh_cache`) — lint-gated
- `mesh_cache`: `store_with_id` + `get_captured` (kept `get_raw`/`store`).
- `mesh_files()` bitcode-encodes each `AssetSource::Mesh` to `assets/<id>.mesh.bin`;
  `save_to_dir` writes them; `restore_mesh_bytes()` reloads into the store **before**
  `apply_project` rebuilds the scene — wired into the dir + URL loaders.

### 🟡 Commands + bridge re-materialize — lint-gated
- `ConvertToEditableMesh { node, mesh }` (caller-minted id; bakes Primitive/Sweep →
  editable mesh, swaps to `NodeKind::Mesh`, carries material slots; inverse =
  `Batch[SetKind(prior), DeleteAsset(mesh)]`). `SetMeshData { mesh, data }` (whole
  replace; inverse restores prior `CapturedMesh`). `affects_mesh()` + `mesh_revision`
  counter. `bridge::mesh_sync` re-materializes Mesh nodes on a bump. MCP
  `convert_to_editable_mesh`.

#### Morning checklist (browser / MCP)
1. Insert a Box → `convert_to_editable_mesh { node }` → `get_node_details` shows
   `NodeKind::Mesh` → save to a dir → confirm `assets/<id>.mesh.bin` exists →
   reload (or load_project_from_url) → still renders (`screenshot_scene`/`canvas_stats`).
2. `SetMeshData` (via run_command/dispatch) with edited positions → confirm the
   mesh re-materializes (mesh_sync) and `get_node_bounds` reflects it → undo restores.

#### ⬜ Phase 2 remaining (browser-only)
- **Player loader parity**: the game-runtime player also reads `AssetSource::Mesh`.
  The side-file scheme (`assets/<id>.mesh.bin`, bitcode `CapturedMesh`) is symmetric,
  but confirm the player has (or gains) a loader for it — likely net-new on the
  player side. Don't break the player's existing project load.

## Phase 3 — Procedural modifier stack — 🟡 core done (native-tested), browser-pending

### ✅ Schema (`scene-schema/src/modifier.rs`) — native-tested
- `ModifierStack { base, modifiers }`; `MeshBase::{Primitive,Lathe,Superquadric,
  Sweep,Captured}`; `Modifier::{Taper,Twist,Bend,Inflate,Spherify,Roughen,
  Subdivide,Smooth,Mirror,Array,Displace}` + `Axis`. On `MeshDef.modifiers`
  (`#[serde(default)]`). Round-trip test (serde + bitcode) + default.

### ✅ Evaluation (`meshgen/src/modifiers.rs`) — native-tested (14 tests)
- `evaluate(&ModifierStack)` + `apply_modifiers(base, &[Modifier])`. Bases:
  `primitive_mesh`, `lathe` (revolve a (height,radius) profile), `superquadric`.
  Deformers implemented; `Displace{expr}` is a no-op pending an expression
  evaluator. `meshgen` now depends on `scene-schema` (no cycle).

### 🟡 Command wiring — lint-gated
- `SetMeshModifiers { mesh, stack }` (whole-stack replace; inverse = prior stack
  or prior bytes). `controller/mesh_eval::evaluate_stack` resolves Sweep/Captured
  bases against the scene then applies deformers. Stores the recipe on
  `MeshDef.modifiers`, re-bakes the cache, bumps `mesh_revision`. MCP
  `set_mesh_modifiers` (mesh id + ModifierStack JSON).

#### Morning checklist (browser / MCP)
1. `convert_to_editable_mesh` a box → `set_mesh_modifiers` with a `twist` →
   `screenshot_scene`/`get_node_bounds` shows the deformation → `undo` restores.
2. `set_mesh_modifiers` a `lathe` with a `(height,radius)` bat profile →
   confirm a bat-like silhouette renders.

#### ✅ `Displace{expr}` evaluator — native-tested
- `meshgen/src/expr.rs`: self-contained recursive-descent evaluator (no deps) over
  `(x,y,z,nx,ny,nz,u,v,i,pi,tau)` + `sin/cos/tan/abs/sqrt/floor/sign`. `Displace`
  compiles once + offsets each vertex along its normal; malformed → no-op.

#### ✅ Introspection (`get_mesh_stats` / `get_mesh_cross_section`) — native-tested
- `meshgen/src/stats.rs`: `mesh_stats` (counts/bbox/centroid/area/volume/watertight,
  position-welded) + `cross_section_profile` (silhouette radius along an axis).
- `EditorQuery::MeshStats` / `MeshCrossSection` (resolve geometry via the shared
  `export::node_mesh`) + MCP `get_mesh_stats` / `get_mesh_cross_section`. Closes
  the agent measure→adjust loop. **Browser:** verify a lathe bat profile's barrel
  radius via `get_mesh_cross_section`.

#### ⬜ Phase 3 remaining
- **Coalescing**: `SetMeshModifiers` should coalesce per-mesh into one undo entry
  (like `SetCustomMaterialLayout`) — add a coalesce key (see the coalesce-key fn
  in `controller/state.rs`, currently keyed by `NodeId`; needs an `AssetId`-keyed
  variant). Skipped to avoid touching that machinery blind.
- **Recipe seeding on convert**: `ConvertToEditableMesh` currently bakes to bytes
  with `modifiers: None`; could seed `Some(ModifierStack{ base: <originating>,
  modifiers: [] })` once eval is the materialization path (so adding a modifier
  to a just-converted primitive Just Works).
- ✅ **Convenience MCP** (DONE + MCP-verified): `add_modifier` / `set_modifier`
  / `remove_modifier` commands (read-modify-write on an existing stack; clear
  error if the mesh has no recipe — never synthesizes a circular `Captured`-self
  base; bounds-checked) + a `get_mesh_modifiers` query, all on top of a factored
  `apply_mesh_stack` helper (inverse = `SetMeshModifiers(prior)`), exposed as
  MCP tools + documented in `awsm://docs/mesh-tools`. Verified live via
  `dispatch_command` + `get_mesh_stats`: no-stack→error; add subdivide(2)+taper
  → 24→150 verts, volume halved; remove taper → volume recovered, subdivide
  kept; set_modifier subdivide 2→1 → 192→48 tris; index 9 → out-of-range error;
  undo → restored 192 tris.
- **Bridge materialization via eval**: today Mesh nodes materialize from the
  baked `.mesh.bin` (`mesh_cache::get_raw`); the recipe is re-baked at edit time
  in the command. Confirm in-browser that this is the desired path (vs. the
  bridge evaluating the recipe itself on load).

## Phase 4 — Raw per-vertex editing — 🟡 geometry core done (native-tested)
### ✅ `meshgen/src/edit.rs` — native-tested (5 tests)
- `soft_transform_positions` (smoothstep falloff over a radius; hard move when
  falloff ≤ 0) + predicate selection (`select_by_normal_dir`, `select_by_axis`,
  `select_top_percent_axis`, `select_within_radius`). Pure functions — the heart
  of the future commands.
### 🟡 Commands + MCP — lint-gated
- `SetVertexPositions` (sparse inverse), `SoftTransformVertices` (server falloff
  via `edit::soft_transform_positions`; sparse inverse over moved verts),
  `CollapseMeshStack` (bake recipe → raw; inverse = Batch[SetMeshModifiers(prior),
  SetMeshData(prior_bytes)]). All in `affects_mesh` → mesh_revision re-materialize.
  MCP `set_vertex_positions` / `soft_transform_vertices` / `collapse_mesh_stack`.
- Browser: `collapse_mesh_stack` → `select` (by index) → `soft_transform_vertices`
  → `get_node_bounds` reflects the move → undo restores exactly.
### 🟡 Predicate selection — lint-gated
- `EditorQuery::SelectVerticesWhere { node, predicate }` → matching vertex
  indices, via the tested `edit::select_*` fns. `VertexPredicate`: normal_dir /
  axis_greater / axis_less / top_percent / within_radius. MCP
  `select_vertices_where`. Closes the cursor-free loop with the introspection
  queries + soft_transform_vertices.
### Phase 4 remaining
- ✅ **Vertex-selection highlight** (DONE + browser-verified): transient
  `SetVertexSelection { node, indices }` command + controller `vertex_selection`
  field (like `SetSelection`); a read-only bridge observer
  (`engine/bridge/vertex_highlight.rs`) draws an amber 3-axis cross at each
  selected vertex (world-space, sized to the mesh bbox), torn down + rebuilt on
  change. MCP `set_vertex_selection` (pairs with `select_vertices_where`).
  Verified live: selected a sphere's top-cap verts → amber crosses render at the
  pole ring; empty selection clears them. (First-cut: markers are baked at
  selection-time world matrix — re-emit after moving the node.)

## Phase 5 — SDF / CSG — 🟡 core complete (native-tested)
- `MeshBase::Sdf { node, resolution }` + `SdfNode`/`SdfPrimitive` (round-trip
  tested). `meshgen/src/sdf.rs`: `eval_sdf` distance graph (smooth booleans) +
  `sdf_bounds`. `sdf_mesh.rs`: `surface_nets_mesh` via `fast-surface-nets`
  (sphere + CSG mug tested). `evaluate(Sdf)` returns real geometry, so
  `set_mesh_modifiers` with an SDF base meshes it.
- ⬜ Browser: `set_mesh_modifiers` a mug SDF graph → `screenshot`/`get_mesh_stats`
  shows a closed rounded result; tune `resolution` + grid margin.

## Phase 6 — Player runtime bundle — 🟡 first-cut wired (lint-gated)
- `EditorQuery::ExportPlayerBundle { name }` returns a manifest: base64
  `scene.glb` (whole-scene `export_glb` — geometry + materials + lights/cameras),
  pruned custom-material side-files (`persistence::material_files`), and an env
  descriptor. MCP `export_player_bundle`. (Confirms the Phase-1 scene-complete IR
  reuses with no rewrite.)
### Phase 6 remaining
- ✅ **Animation lowering** (DONE — `79f9fc8b`): editor TRS clips → glTF channels
  in `GlbScene.animations` (`KHR_animation_pointer` for material/light/camera
  tracks is a follow-on).
- ✅ **Bundle → dir** (DONE + tested): native `awsm_glb_export::assemble_bundle`
  + `PlayerBundle::write_to_dir` (tempdir integration test, `43668889`); the
  editor's `assemble_player_bundle` reuses it (no layout drift) and the overflow
  menu "Export player bundle…" writes the file set to a picked `ProjectDir`.
  `bundle.json` indexes `scene.glb` + material side-files + `textures/` + env.
  MCP-verified: `run_query export_player_bundle` returns the correct file set
  (scene.glb `glTF…`, env.json, bundle.json manifest).
- ✅ **Texture copying**: PBR textures travel embedded in `scene.glb`
  (referenced-only); custom-WGSL material textures are gathered into `textures/`
  by `assemble_player_bundle`. Follow-on: declared-slot defaults (un-overridden)
  + compression.
- ⬜ Browser: confirm the "Export player bundle…" dir-write picks a dir + writes
  the files (FS-Access; the manifest assembly is MCP-verified + the layout
  tempdir-tested).
- **Out of scope (handoff):** the **player-side bundle loader** lives in the
  separate game-player repo — it consumes `bundle.json` + `scene.glb` +
  `materials/` + `textures/` + `env.json`.

## Generated capabilities reference / `awsm://docs/mesh-tools` — ⬜ NOT STARTED
Mesh-edit view is read-only + a generated reference (no manipulation UI).
