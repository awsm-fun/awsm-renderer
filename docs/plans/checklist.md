# Road to 100%: feature-complete + verified

The tracked checklist to finish the unified mesh-convert architecture, skins/morphs,
correct rendering, and full testing. Companion to `docs/plans/mesh-pipeline-overhaul.md`
(history/rationale) and `docs/buffers.md` (architecture). Branch `mesh-authoring`.

Legend: **cargo** = verifiable with `cargo test`/`clippy`; **browser** = needs an
in-browser render/MCP check. Check items off as completed.

## Already done (context)
- `awsm-gltf-convert` crate FEATURE-COMPLETE + proptested: geometry (canonical glb +
  baked tangents + `AWSM_format`, idempotent) + materials (PBR + all KHR extension
  factors + extension texture refs) + animations + images (GLB + `data:`-URI).
- `awsm-tangents` shared crate (renderer + glb-export); `glb-export` bakes `TANGENT`.
- `renderer::mesh_pack` shared packer (raw-mesh paths route through it).
- Session render fixes (transmission/tangent/shadow; env-from-URL MCP) — browser-verified.
- `docs/buffers.md`, `docs/iridescence-analysis.md`.

---

## 1. Wire the convert pipeline in — KEYSTONE (everything downstream depends on this)
The convert crate is built + tested but **unused**. Wiring it is the payoff.

- [ ] **Mapping layers** — `convert`'s neutral specs → app types: `MaterialSpec` →
  editor-protocol `MaterialDef` (editor) AND `scene::MaterialDefinition` (player);
  `AnimationSpec` → clips; `ImageData` → textures. Unit-test each. **[cargo]**
- [ ] **Player wiring** — `scene-loader` calls `convert()` before populating; map specs;
  loads a foreign glTF and a canonical glb identically. **[cargo + browser]**
- [ ] **Editor wiring** — import → `convert()` → eager editable `MeshData`; DELETE the
  populate-then-hide (`gltf.rs:284/290`); export stamps `AWSM_format`. **[cargo + browser]**
- [ ] **Phase 2b — route `renderer-gltf` through `mesh_pack`** (thread `front_face`;
  decode attribute byte-maps → typed slices) so editor + player share ONE packer.
  Byte-parity test. **[cargo(wasm) + browser]** ⚠️ gltf hot path — verify a render.

## 2. Verify tonight's safe changes render right **[browser]**
- [ ] `mesh_pack` refactor (`555cee5a`) — a normal model + a glass model still render right.
- [ ] `glb-export` TANGENT baking (`16a92110`) — editor export → player round-trip looks right.

## 3. Dish / KHR-material shading fix (analysis in `docs/iridescence-analysis.md`)
- [ ] Replace the 3-wavelength two-beam thin-film approx in `brdf.wgsl` with the spec's
  `evalSensitivity` spectral→RGB (Khronos sample-viewer approach). **[browser]**
- [ ] Verify transmission↔reflection energy conservation at grazing ("white bowl top"). **[browser]**
- [ ] Match `olives.png` (clear glass + gold metal + subtle pink iridescence) under a matching IBL.
- [ ] Sweep other KHR-extension models (clearcoat/sheen/anisotropy/…) vs Khronos refs.

## 4. Skins & morphs first-class via MCP (priority)
Backend (command layer **[cargo]**; correctness **[browser]**):
- [ ] `get_morph_data` / `get_skin_data` read-back queries.
- [ ] `set_morph_weight(node, index, value)` — live morph weight.
- [ ] Skin joint-weight editing + bind-pose / inverse-bind editing.
- [ ] Richer skeletal/morph animation authoring via MCP.
- [ ] Evaluate + wire third-party crates (IK, weight-smoothing/normalization, retargeting).

Visualization (Phase 6, editor UI **[browser]**):
- [ ] Bone icons in the outliner for joint/skin nodes.
- [ ] Skeleton (bone-line) + morph visualization, incl. during animation playback.

## 5. Round-trip completeness — import → edit → export → re-import/play, faithful for all:
- [ ] Static meshes (primitives, captured, multi-primitive/multi-material — DON'T merge).
- [ ] Skinned meshes.
- [ ] Morph-target meshes (bundle exporter currently "static for now" — finish it).
- [ ] All materials + KHR extensions + textures (samplers, `KHR_texture_transform`).
- [ ] Animations (transform + morph + skeletal), cameras, lights, environment/IBL.
- [ ] Vertex colors, tangents, all UV sets.

## 6. Testing to 100%
- [ ] Editor/player **mapping** proptests + **mesh_pack parity** test (after Phase 2b). **[cargo]**
- [ ] **Golden-image / GPU-readback** verification for a model matrix through the new
  unified path (certifies "renders correctly", not just "round-trips"). **[browser]**
- [ ] **In-browser round-trip harness** — import → export → re-import → second render
  matches first, across the content matrix. **[browser]**
- [ ] Convert edge-cases — extension `TexRef` sampler + `KHR_texture_transform`.  **[cargo]**
- [ ] Final Phase 7 sweep — doc/MCP-tool fidelity, workspace clippy, dead-code cleanup.

---
**Critical path:** #1 (wiring) unblocks everything → #2 + #5 + #6 run together → #3
(shading) and #4 (skins/morphs) are independent tracks. Work top-down; check items off.
