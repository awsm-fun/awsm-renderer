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

## Phase 2 — Editable mesh asset + persistence — ⬜ NOT STARTED
Next up. Plan (spec §"Phase 2"):
- `scene-schema/src/material.rs`: extend `MeshDef` with `#[serde(default)] editable: bool`;
  add `CapturedSource::Editable`/`Imported`. Native test: round-trip + default.
- `controller/persistence.rs`: `mesh_files()` (binary sibling of `material_files`);
  `save_to_dir` writes `assets/<id>.mesh.bin` (bitcode); `load_*` reads them into a
  persisted mesh store (keep `mesh_cache`'s `get_raw`/`store` API so `node_sync` is
  untouched). Closes the session-local-only gap.
- Commands `ConvertToEditableMesh { node, mesh }` + `SetMeshData { mesh, data }`;
  bridge **mesh-revision counter** (mirror `affects_animation`) so no edit skips
  re-materialize. MCP `convert_to_editable_mesh`.
- Risk: the game-runtime **player** also reads `AssetSource::Mesh` — keep the
  side-file scheme symmetric (player loader parity).

## Phases 3–6 — ⬜ NOT STARTED
- 3: procedural modifier stack (`scene-schema/src/modifier.rs` + `meshgen/src/modifiers.rs`,
  native per-modifier tests). 4: raw per-vertex editing. 5: SDF/CSG (surface-nets crate).
  6: player runtime bundle (reuses `write_glb`/`GlbScene`).
- Capability menu is incremental by cost tier, not phase-gated.

## Generated capabilities reference / `awsm://docs/mesh-tools` — ⬜ NOT STARTED
Mesh-edit view is read-only + a generated reference (no manipulation UI).
