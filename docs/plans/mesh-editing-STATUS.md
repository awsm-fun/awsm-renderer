# Mesh-editing + GLB-export — STATUS

Running log of the mesh-editing/GLB-export arc (spec: `docs/plans/mesh-editing.md`).
Branch: `mesh-authoring`. Native gates used each commit: `task lint` (fmt + clippy
`-D warnings`, whole workspace incl. tests) + relevant `cargo test`.

Legend: ✅ done & gated · 🟡 implemented, needs in-browser verification · ⬜ not started.

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
- **UI button**: "Export GLB" in the inspector header + per-node export, writing
  via `ProjectDir::write_bytes` (`fs.rs:151`) or a blob download. Pattern: the
  "Capture as Mesh" button at `scene_mode/inspector.rs:~276`.
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
- **Convenience MCP**: `add_modifier` / `set_modifier_param` (read-modify-write
  one stack) on top of `set_mesh_modifiers`.
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
### ⬜ Phase 4 remaining (editor — lint-only / browser)
- Commands `CollapseMeshStack` (bake modifiers → raw, clear stack; heavy
  snapshot inverse), `SetVertexPositions { mesh, indices, positions }` (sparse
  inverse = prior positions of touched verts only), `SoftTransformVertices`
  (wrap `edit::soft_transform_positions` → sparse `SetVertexPositions`).
  Transient `SetVertexSelection` (+ `select_vertices_where` mapping a predicate
  enum to the `edit::select_*` fns). MCP tools. Read-only selection-highlight
  rendering in the bridge/viewport (the one small view addition).
- **5** SDF/CSG: `MeshBase::Sdf(SdfNode)` + a surface-nets crate in `meshgen/src/sdf.rs`.
- **6** player runtime bundle: `ExportPlayerBundle` reusing `write_glb`/`GlbScene`
  (the IR is already scene-complete — lights/cameras/animations/env slots exist).
- Capability menu is incremental by cost tier, not phase-gated.

## Generated capabilities reference / `awsm://docs/mesh-tools` — ⬜ NOT STARTED
Mesh-edit view is read-only + a generated reference (no manipulation UI).
