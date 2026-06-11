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
- [x] `mesh_pack` refactor (`555cee5a`) — behavior-preserving (confirmed once the
  mask bug below was understood; mesh_pack didn't cause it).
- [ ] `glb-export` TANGENT baking (`16a92110`) — editor export → player round-trip
  looks right (verify alongside the round-trip harness, #5/#6).

## 2b. MASK-as-alpha-tested-opaque (the floor-through-bowl bug → proper fix)
Mask was routed to the transparent pass, so it was absent from `opaque_tex` (the
framebuffer transmission samples) AND didn't cast shadows. Proper fix = MASK is
alpha-tested OPAQUE (deferred), with alpha-test in the raster. Decided: deferred
alpha-test in the visibility raster (only masked meshes pay the base-color-alpha
texture lookup, via a `geometry` pipeline variant).
- [x] **Step A** (`fix(renderer): route MASK … step A`) — mask → Visibility/opaque
  (`is_transparency_pass` drops alpha_cutoff for PBR; `mesh_buffer_geometry_kind`
  Mask→Visibility). VERIFIED: dish bowl now solid gold (no floor-through), mask
  casts shadows, console clean. Renders mask SOLID (no cutout) until B.
- [ ] **Step B1** — masked `geometry` raster variant: bind material+texture pool+
  attribute (UV) buffers, sample base-color alpha at the fragment UV (read UV via
  triangle_index+barycentric like the opaque compute), `discard` if `< cutoff`.
  Cache-key `alpha_test` flag → only masked meshes use it. Verify on a cutout-mask
  model (foliage).
- [ ] **Step B2** — same alpha-test in the SHADOW raster variant (else cutout
  masks cast solid/rectangular shadows).
- [ ] **Step B3** — dynamic/custom material mask: route `Material::Custom` mask →
  visibility (`materials.rs:135`); masked variant alpha-tests using the custom
  WGSL's alpha output; editor UI toggle + MCP tool to set `alpha_mode=Mask`+cutoff.
- [ ] **Sweep** — `is_transparency_pass` call sites, `docs/buffers.md` geometry-kind
  table, raw_mesh/mesh_pack/geometry comments. Comprehensive.

### B IMPLEMENTATION MAP (do B1 + B3 together; TEST WITH A DYNAMIC MATERIAL FIRST)
USER DIRECTIVE: implement the masked-raster alpha-test (B1) AND the dynamic-material
cutoff flag (B3) in the same step, and **build the test case with a DYNAMIC (custom
WGSL) material first** — it's the easiest way to construct controlled cutout +
shadow test cases (author a custom material whose alpha is a known pattern, e.g. a
checker / radial cutout, apply to a plane, verify holes are see-through, cast
hole-shaped shadows, and let transmission show through the holes).

STATE: Step A done+committed+VERIFIED (`fix(renderer): route MASK … step A`):
- `materials/src/pbr.rs:490` `is_transparency_pass` = `has_alpha_blend()||has_transmission()` (Mask dropped).
- `renderer-gltf/src/buffers/mesh.rs` `mesh_buffer_geometry_kind`: Mask → Visibility.
- Mask now renders SOLID opaque (no cutout yet); dish bowl fixed (goldLeaf solid).

B1 — masked `geometry` raster variant (alpha-test discard). Files under
`renderer/src/render_passes/geometry/`:
- `shader/cache_key.rs` `ShaderCacheKeyGeometry` — add `alpha_test: bool` (or
  `masked`). Distinct pipeline only for masked meshes (only they pay the cost).
- `shader/geometry_wgsl/{vertex,fragment}.wgsl` + `shader/template.rs` — masked
  variant: read UV from the custom-attribute buffer via `triangle_index`+
  `barycentric` (the visibility vertex stream has NO uv — confirmed), sample
  base-color `.a` × base-color-factor `.a`, `discard` if `< alpha_cutoff`. MIRROR
  the opaque compute's `_pbr_material_base_color` in
  `material_opaque/shader/material_opaque_wgsl/helpers/material_color_calc.wgsl`
  (uses `attribute_data_offset` + `triangle_indices` + `vertex_attribute_stride` +
  `material.base_color_tex_info`). Reuse the shared `textures`/material wgsl modules.
- `bind_group.rs` + `pipeline.rs` — the masked variant binds the MATERIAL buffer +
  TEXTURE POOL + ATTRIBUTE buffers (which the opaque compute already binds; copy
  that layout). Opaque variant keeps its cheap no-sampling bind group.
- Per-mesh pipeline selection: route masked meshes → masked geometry pipeline (see
  the geometry `render_pass.rs` + how it picks pipelines per mesh; material's
  `alpha_cutoff()` is the signal).
- Opaque compute already shades mask (it's now visibility) — verify no change needed
  (alpha_cutoff is moot in shading; discard happened in raster).

B2 — SHADOW raster alpha-test (`renderer/src/shadows/shader/` — currently NO discard,
so masked meshes cast SOLID shadows). Add the same UV+base-color-alpha discard to the
shadow raster's masked variant, else cutout masks cast rectangular shadows.

B3 — dynamic/custom material mask (do WITH B1):
- `materials/src/materials.rs:~135` `Material::Custom is_transparency_pass` =
  `matches!(m.alpha_mode, Blend|Mask)` → change to exclude Mask (route Custom mask →
  visibility, consistent with PBR). Dynamic alpha_mode already exists
  (`scene/src/dynamic_material.rs:63`, incl `Mask`).
- The masked geometry variant must alpha-test using the CUSTOM material's alpha
  output (custom WGSL computes alpha; the variant discards on it + the cutoff). May
  need the custom fragment's alpha exposed to the geometry masked variant — design
  this (the custom shading is in `dynamic_materials`/`material_*` — check how custom
  materials' fragment alpha is available; possibly run the custom alpha calc in the
  masked geometry variant).
- Editor: add an alpha_mode=Mask + cutoff toggle to the dynamic-material UI
  (`frontend/editor` material inspector) + an MCP tool (`set_material_alpha_mode`
  already exists — check it covers Mask+cutoff for dynamic materials; `mcp/src/mcp.rs`).

KEY ARCH FACTS (verified this session):
- `opaque_tex` (transmission's background) = opaque RT mip chain built at
  `renderer/src/render.rs:~917`, BEFORE the transparent pass. Mask must be in the
  opaque RT by then (Step A achieves this).
- transmission samples `opaque_tex` in
  `material_transparent/.../fragment.wgsl` (`sample_transmission_background`).
- geometry fragment currently writes only visibility data (triangle id, bary,
  normal/tangent) — `geometry_wgsl/fragment.wgsl`. No texture access today.

FINALIZED B DESIGN (validated against code, this session — supersedes ambiguities above):
- WHY raster (not compute) discard: the visibility raster writes DEPTH. If discard
  happened only in the opaque COMPUTE (after geometry), the hole's depth is already
  written → later depth-tested geometry/shadows/transmission can't see through the
  hole. So the discard MUST be in the raster. Confirmed: no compute-side shortcut.
- maxBindGroups = 4 (macOS Metal ceiling; geometry already uses all 4: 0 camera+
  frame_globals, 1 transforms, 2 meta, 3 animation). SOLUTION: the masked variant
  does NOT add a 5th group — it APPENDS its fragment-only bindings onto GROUP 0
  (already F-visible) as a DISTINCT group-0 layout: `materials`(storage),
  `material_mesh_metas`(storage), the merged geometry pool `visibility_data`(storage),
  `texture_transforms`(storage), texture pool arrays+samplers, (+extras_pool/
  instance_attrs for custom). Vertex path + shared morph/skin/meta helpers
  (groups 1–3) are UNTOUCHED → low risk. Per-stage storage-buffer budget stays <8.
- The masked variant is SPECIALIZED PER shader_id (mirrors the opaque compute's
  `ShaderCacheKeyMaterialOpaque`), because the geometry template canNOT include the
  full `{{ materials_wgsl }}` blob (it pulls dynamic-material fragments that
  reference opaque-only contract types `OpaqueShadingInput` etc.). So
  `ShaderCacheKeyGeometry` gains an `alpha_test: Option<…>` carrying shader_id/base/
  pool-lens/(dynamic info). Non-masked meshes keep `None` (one bool's worth of cost,
  no texture lookup). Builtin (PBR/Unlit/Toon) masked variants emit just that
  material's base-color-alpha load; custom emits the alpha-only fragment (B3).
- Material buffer carries the cutoff for BUILTIN: `pbr_material.wgsl` header after
  shader_id = [alpha_mode(u32), alpha_cutoff(f32), base_color_tex(5), base_color_factor(4), …]
  (`materials/src/wgsl/pbr/pbr_material.wgsl:110` `pbr_get_material`; written at
  `materials/src/pbr.rs:505-512`). Masked PBR fragment: read alpha_cutoff + base_color
  → `color=base_color_factor; if base_color_tex.exists { color*=texture_pool_sample(uv) }`;
  `if color.a < alpha_cutoff { discard; }`. UV via `texture_uv(attribute_data_offset,
  triangle_indices, bary, tex_info, stride, uv_sets_index)` reading the merged pool
  (mirror compute.wgsl:128-140 reconstruction of triangle_indices + the offsets from
  `material_mesh_metas[material_mesh_meta_offset/256]`). The masked vertex shader
  forwards `material_mesh_meta_offset` (already a flat varying) + triangle_index +
  barycentric to the fragment.
- B3 alpha-only custom (USER-CLARIFIED): a custom material whose alpha mode = cutoff
  gets a SECOND WGSL editor window that returns `alpha: f32`. This second fragment is
  wrapped + compiled into the masked visibility variant (NO lighting/brdf), and
  OPTIONALLY binds textures (procedural cutoff → near-zero cost; texture cutoff → one
  sample). The second window + its templating only EXIST when alpha mode = cutoff is
  selected for the material. So the dynamic registration carries an optional
  `alpha_wgsl: Option<String>` (present iff alpha_mode=Mask). The masked variant for
  that custom shader_id wraps it as `fn custom_alpha_dynamic(AlphaOnlyInput) -> f32`
  and discards if `< cutoff`. Reuse the generated `MaterialData` + `material_sample_*`
  helpers. Gate the texture-pool binding on whether the layout has any textures
  (skip for purely-procedural). Cutoff for custom is host-side only today
  (`materials.rs:152`) and NOT in the GPU buffer → plumb it into the masked custom
  variant (decide at impl: material-buffer prefix or per-mesh uniform). Route
  `Material::Custom` mask → visibility: `renderer/src/materials.rs:135`
  `is_transparency_pass` drop `Mask` from the Custom arm (keep `Blend`).
- Per-mesh routing: `renderable.rs:172` collection — add the masked signal via
  `material.alpha_mask().is_some()` (renderer `materials.rs:146`) into
  `GeometryRenderPipelineKeyOpts`; `meshes/mesh.rs::push_geometry_pass_commands`
  binds the augmented group-0 for masked draws.
- IMPLEMENTATION ORDER (USER-CONFIRMED dynamic-first): (1) masked geometry variant
  machinery (group-0 augmentation, per-shader-id specialized cache key + pipeline +
  bind group + template). (2) custom alpha-only contract (`alpha_wgsl`) + wrap into
  the masked variant + route `Material::Custom` mask→visibility + minimal masked
  routing → browser-verify a PROCEDURAL dynamic cutout on a plane (holes
  see-through), then a TEXTURE-based dynamic cutout (separately, to exercise both
  paths). (3) editor: second WGSL window shown only when alpha-cutoff selected +
  MCP to set it. (4) B2 shadow masked variant → hole-shaped shadows; then
  transmission-through-holes. (5) PBR masked arm (minimal base-color alpha) →
  browser-verify a PBR cutout. (6) sweep. RATIONALE: PBR `MASK` meshes stay on the
  existing non-masked geometry pipeline (render SOLID, = step-A behavior) until step
  5, so dynamic-first is regression-free + incremental.

DEV STACK / TEST SETUP (this session):
- Trunk's file-watch went stale mid-session; FIX = restart `task mcp-dev` (kills+
  restarts trunk:9085 + media:9082/3 + MCP:9086; editor browser reconnects on
  reload). After a renderer/materials/glb-export change, the NEW trunk DOES rebuild
  (it watches those); a change to renderer-gltf/gltf-convert/tangents alone is NOT
  watched → touch a watched file (e.g. `renderer/src/lib.rs`) to trigger.
- Verify dish: import `http://localhost:9082/glTF-Sample-Assets/Models/IridescentDishWithOlives/glTF/IridescentDishWithOlives.gltf`;
  `set_environment` skybox/ibl_prefiltered/ibl_irradiance =
  `https://dakom.github.io/awsm-renderer-assets/photo_studio/{skybox,env,irradiance}.ktx2`;
  orbit `yaw 0.7 pitch 0.12 radius 0.34 look_at [0,0.03,0]` for the bowl close-up.
- For B testing: author a custom dynamic material with a cutout alpha pattern via
  MCP (`add_custom_material` + `set_material_wgsl`), apply to a plane, verify holes
  see-through + hole-shaped shadows + transmission-through-holes.

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
