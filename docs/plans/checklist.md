# Road to 100%: feature-complete + verified

The tracked checklist to finish the unified mesh-convert architecture, skins/morphs,
correct rendering, and full testing. Companion to `docs/plans/mesh-pipeline-overhaul.md`
(history/rationale) and `docs/buffers.md` (architecture). Branch `mesh-authoring`.

Legend: **cargo** = verifiable with `cargo test`/`clippy`; **browser** = needs an
in-browser render/MCP check. Check items off as completed.

## ▶▶ RESUME STATE (cutout + AA phase — branch mesh-authoring)
DONE + COMMITTED + BROWSER-VERIFIED (live MCP) + clippy/fmt-clean:
- B1 PBR cutout (masked visibility raster): DiffuseTransmissionPlant leaves cut out.
- B3 custom cutout: a custom material's 2nd "alpha-only" WGSL window (gated on
  alpha_mode=Mask); procedural stripe cutout verified see-through. Custom MASK routes
  opaque + masked-raster cutout. MCP `set_material_alpha_wgsl` + editor 2nd-WGSL pane.
- MSAA cutout anti-aliasing: the masked fragment evaluates the masking alpha at each of
  the 4 MSAA sample sub-positions (bary screen-space derivatives) → `@builtin(sample_mask)`
  per-sample coverage → the EXISTING compute MSAA edge-resolve blends it. Works for binary/
  procedural alpha (the analytic fwidth approach did NOT — gradient-free). Verified smooth.
  Gated to MSAA-on × cutout-materials; non-cutout opaque skips it entirely. Documented in
  docs/buffers.md ("Masked materials" + the no-TAA promo angle).
- BUG FIXED (caught only in-browser): editor `build_registration` wgsl_hash hashed ONLY the
  main WGSL → editing alpha_mode/alpha_wgsl was a no-op (stale registration → masked never
  built). Now folds alpha_mode+cutoff+alpha_wgsl into wgsl_hash.

KEY FILES: renderer `render_passes/geometry/{masked_bind_group,masked_pipeline}.rs` +
`shader/{masked_cache_key,masked_template}.rs` + `shader/masked_wgsl/{bind_groups,fragment}.wgsl`;
`materials.rs` (alpha_cutoff/canonical_shader_id + Custom is_transparency_pass flip);
`textures.rs finalize_gpu_textures` (masked PBR + custom build, `masked_dynamic_dirty` flag);
`renderable.rs` (geometry_masked_render_pipeline_key routing); `meshes/mesh.rs`
(push_geometry_pass_commands `masked` param); `meshes/meta/material_meta.rs` (alpha_cutoff @idx21);
editor `controller/custom_material.rs`+`persistence.rs`+`material_mode/studio.rs`+`engine/bridge/dynamic.rs`;
`editor-protocol/{command,project}.rs`; `mcp/src/mcp.rs`.

DONE (Unlit/Toon sweep, committed, cargo+clippy clean): `materials/src/{unlit,toon}.rs`
is_transparency_pass now `has_alpha_blend()` (MASK→opaque like PBR); `textures.rs` finalize
builds masked variants for PBR/UNLIT/TOON (they share the header prefix exactly, so the
masked fragment's `{% else %}` base-color path covers all three — no WGSL change). NOT
separately browser-tested (identical code path to verified-PBR, differs only by shader_id;
a real Unlit/Toon cutout needs a base-color texture with an alpha pattern to be visible).

REMAINING: FlipBook masked (atlas-cell alpha, DEFERRED — mask alpha is the time-varying
sprite cell; flipbook.rs left transparent-routed); B2 shadow masked variant
(hole-shaped shadows — shadow pass is depth-only + at maxBindGroups=4, needs a bind-group
consolidation, see B2 EXECUTION PLAN below); textured-CUSTOM cutout browser test
(material_sample_<name> path — PBR-textured already verified); scene-loader player round-trip
of alpha_wgsl (currently None); editor Mask contract docs (main WGSL now OpaqueShadingOutput).

DEV STACK: `task mcp-dev` runs in a background Bash task (id bf79wbng1) — trunk:9085 (editor,
auto-rebuilds on renderer/editor save + live-reloads browser), MCP:9086 (`cargo run`, rebuilds
on mcp/editor-protocol change), media:9082/3. Editor URL: http://localhost:9085/?mcp=http://127.0.0.1:9086.
Verify via MCP: ping → insert_primitive/import_model_from_url → set_material_* → screenshot_scene.
KNOWN LIMITATION: masked pipelines (re)build on texture-finalize, not on MSAA toggle.

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
- [x] **Step B1** — masked `geometry` raster variant (built-in PBR): binds material+
  texture pool+attribute (UV) buffers on an augmented group 0, samples base-color alpha
  at the fragment UV (UV via triangle_index+barycentric like the opaque compute),
  `discard` if `< cutoff` (per-mesh cutoff in MaterialMeshMeta). Separate per-shader-id
  masked pipeline pool; PBR built on texture-finalize. COMPILES + clippy-clean.
  ▶ BROWSER-VERIFY (pending user): DiffuseTransmissionPlant `leaves` leaf-shaped.
- [ ] **Step B2** — same alpha-test in the SHADOW raster variant (else cutout
  masks cast solid/rectangular shadows). NOT YET DONE — see the B2 EXECUTION PLAN.
- [x] **Step B3** — dynamic/custom material mask DONE + clippy/fmt-clean: routed
  `Material::Custom` mask → opaque (`materials.rs`), launch builds its opaque pipeline,
  the masked variant runs the author's 2nd alpha-only WGSL (`custom_alpha_dynamic`),
  `DynamicMaterials::alpha_info_for` + finalize build (with a `masked_dynamic_dirty`
  registration trigger for procedural cutouts), MCP `set_material_alpha_wgsl`, editor
  2nd-WGSL pane (shown when alpha=Mask) + `StoredMaterial.alpha_wgsl` persistence.
  ▶ BROWSER-VERIFY (pending user): a custom procedural cutout on a plane authored via MCP
  (set main WGSL → OpaqueShadingOutput, set_material_alpha_mode mask, set_material_alpha_wgsl
  → e.g. `return select(1.0,0.0,fract(input.uv.x*8.0)<0.5);`). NOTE: a custom MASK
  material's MAIN WGSL now uses the OPAQUE contract (returns OpaqueShadingOutput) since it
  shades in the opaque pass; the cutout lives in the 2nd window. (Editor Mask contract docs
  `AlphaMode::Mask::ret_sig` still say MaskShadingOutput — update as a follow-up.)
  Player round-trip (`scene-loader`) still passes `alpha_wgsl: None` (TODO).
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
BUILD PROGRESS (this session — all COMPILING on `cargo check -p awsm-renderer`):
- ✅ Masked shader: `geometry/shader/masked_cache_key.rs` (ShaderCacheKeyGeometryMasked
  + DynamicAlphaShaderInfo), `masked_template.rs` (reuses the plain geometry vertex,
  renders masked bind_groups + fragment), `masked_wgsl/{bind_groups,fragment}.wgsl`.
  Wired into the `ShaderCacheKeyRenderPass::GeometryMasked` + `ShaderTemplateRenderPass`
  dispatch. Built-in PBR emits a minimal base-color-alpha load; custom emits the
  author's alpha-only fragment (`custom_alpha_dynamic`).
- ✅ Per-mesh cutoff: `MaterialMeshMeta` index 21 (was `_reserved1`) now `alpha_cutoff:
  f32`; written from `Materials::alpha_cutoff(key)` (new helper). WGSL struct updated.
- ✅ Masked bind group: `geometry/masked_bind_group.rs` (`GeometryMaskedBindGroup`) —
  augmented group 0 (camera/frame_globals + materials/material_mesh_metas/merged-pool/
  texture_transforms + texture pool), with `clone_because_texture_pool_changed` +
  `recreate`.
- ✅ Masked pipeline pool: `geometry/masked_pipeline.rs` (`GeometryMaskedPipelines`) —
  lazy `(msaa,shader_id,cull)` map mirroring `MaterialOpaquePipelines`; pipeline layout
  = [masked_group0, transforms, uniform-meta, animation]; `build_descriptors`/`insert`/
  `get`/`clear`/`relayout`. Forces non-instanced uniform-meta path.
- Commits: "masked geometry shader", "masked geometry bind group + lazy pipeline pool".

BROWSER-VERIFIED (this session, live MCP + screenshots):
- ✅ B1 PBR cutout: DiffuseTransmissionPlant `leaves` render leaf-shaped (not solid),
  light through the gaps, diffuse-transmission translucency. Textured PBR cutout works.
- ✅ B3 custom cutout: a procedural custom material (alpha-only WGSL stripe pattern) on a
  plane renders see-through holes. (Textured CUSTOM cutout — `material_sample_<name>` —
  not yet visually tested; PBR-textured proves the texture-pool path.)
- ✅ MSAA cutout anti-aliasing: cutout edges are smooth under MSAA (analytic sample_mask
  coverage → existing compute edge-resolve; no TAA). Documented in docs/buffers.md as a
  promotable property (deferred/visibility-buffer renderers normally need TAA for this).
- 🐛 FIXED (caught only by the in-browser test): the editor bridge's register no-op + the
  registry idempotency keyed on `wgsl_hash`, which hashed ONLY the main WGSL — so editing
  the alpha mode or the 2nd alpha-only WGSL was a no-op (stale Opaque/no-alpha
  registration → masked routing skipped + masked pipeline never built). Fix folds
  alpha_mode+cutoff+alpha_wgsl into wgsl_hash (`editor/.../bridge/dynamic.rs build_registration`).

B1 STATUS = ✅ COMPLETE + COMPILING + CLIPPY-CLEAN (`-D warnings`). PBR glTF MASK
meshes alpha-test in the visibility raster (holes see-through + transmission-through-
holes). Test: import `media/.../DiffuseTransmissionPlant/glTF/DiffuseTransmissionPlant.gltf`
— `leaves` should now be leaf-shaped (not solid rectangles). Wiring landed: construction
threading, recreate dispatch (FunctionToCall::GeometryMasked), finalize PBR pipeline
build (ensure_variant), render+routing (canonical shader_id). ~12 commits this session.
B1 KNOWN LIMITATION: masked PBR pipeline (re)builds on texture-finalize only, NOT on
MSAA toggle — after an MSAA change, masked PBR meshes fall back to solid until the next
texture change. (Fix: also rebuild masked in `set_anti_aliasing`'s recompile path.)

B3 (custom alpha-only) — EXECUTION PLAN (scoped this session):
- `MaterialRegistration` (renderer `dynamic_materials/registry.rs`) gains
  `alpha_wgsl: Option<String>` (Some iff alpha_mode=Mask + author provided). Update the
  5 construction sites: renderer `examples/dynamic_material.rs` (×2), registry test
  `reg()`, `scene-loader/src/dynamic.rs:225`, `editor/.../bridge/dynamic.rs:314`.
- `DynamicMaterials::alpha_info_for(id) -> Option<DynamicAlphaShaderInfo>` (mirror
  `shader_info_for` at registry.rs:662): generate struct/loader/texture_helpers from
  `reg.layout` (`generate_wgsl_struct`/`_loader`/`_texture_helpers`) + `reg.alpha_wgsl`.
- Build masked CUSTOM pipelines: do NOT use the compute-oriented launch scheduler
  (`pipeline_scheduler/launch.rs` issues `createComputePipelineAsync`; masked is a
  RENDER pipeline). Instead extend the finalize-style block: a method that iterates
  registered MASK customs and `ensure_variant`s each (base=Custom, dynamic_alpha=Some).
  Call it from `finalize_gpu_textures` (covers textured customs) AND on custom-material
  registration (covers procedural/no-texture customs) — add a `masked_dynamic_dirty`
  flag set by `register_material`, drained in the render preamble via an async ensure
  step (or reuse the existing post-register pipeline-prewarm path).
- Routing flip: `renderer/src/materials.rs:135` `Material::Custom` is_transparency_pass
  → drop `Mask` (keep `Blend`). The geometry-kind for editor-added custom meshes follows
  is_transparency_pass via `raw_mesh.rs` (verify: it should give Visibility geometry once
  the flip lands — same mechanism step A used for PBR). glTF custom path uses
  renderer-gltf `mesh_buffer_geometry_kind` (already Mask→Visibility).
- Editor 2nd-WGSL window: add `alpha_wgsl` to the editor `CustomMaterial` type + an
  inspector pane shown only when alpha-mode=Mask; thread into `build_registration`.
- MCP: a tool to set a dynamic material's `alpha_wgsl` + alpha_mode=Mask+cutoff
  (`mcp/src/mcp.rs`; `set_material_alpha_mode` exists — extend for dynamic + the 2nd WGSL).
- Test (procedural first): MCP author a custom material whose alpha is a known cutout
  pattern (e.g. `return select(1.0, 0.0, fract(input.uv.x*8.0) < 0.5);`), alpha_mode=Mask,
  apply to a plane → holes see-through; then a texture-based cutout; then shadows (B2);
  then transmission-through-holes.

B2 (shadow masked variant) — EXECUTION PLAN (scoped this session):
- Shadow pass is depth-only (no fragment) and at maxBindGroups=4 (group 0 shadow_view
  dynamic-offset uniform, 1 transforms, 2 meta, 3 animation — render_pass.rs:110-132).
  Need a fragment that samples base-color/custom-alpha + `discard`. The masked-shadow
  fragment needs materials+material_mesh_metas+merged-pool+texture_transforms+pool — but
  all 4 groups are taken, and shadow_view (per-view dynamic offset) can't host them.
  CONSOLIDATION: fold the masked-shadow's material data into a NEW 5th-binding-free
  layout by merging transforms+animation into one group (both vertex storage), freeing a
  group for the augmented material/pool data; OR build a dedicated masked-shadow group
  set. Gate the masked-shadow fragment on `alpha_cutoff` present REGARDLESS of opaque/
  transparent routing (a Mask+refractive material is transparent-routed but must still
  cast a hole-shaped shadow). Add a masked shadow vertex that forwards triangle_index+
  barycentric+material_mesh_meta_offset; masked shadow pipeline pool + render integration
  in `shadows/render_pass.rs` (bind masked groups + masked shadow pipeline for masked
  casters).

REMAINING WIRING (DONE for B1 — kept for history):
1. Hold + construct: add `masked_bind_group: GeometryMaskedBindGroup` + `masked_pipelines:
   GeometryMaskedPipelines` to `GeometryRenderPass` (geometry/render_pass.rs). Build them
   in `RenderPasses::describe_shaders` (render_passes.rs:277 area, after `geometry_bg`)
   and thread through `RenderPassesBindings`/`RenderPassesShaderPlan` → `from_resolved`
   (render_passes.rs:630 `let geometry = GeometryRenderPass { ... }`).
2. Recreate dispatch (bind_groups.rs): add `FunctionToCall::GeometryMasked`; insert it for
   `CameraInitOnly`, `MaterialResize`, `MaterialMeshMetaResize`, `MeshGeometryPoolResize`,
   `TexturePool`, `TextureTransformsResize`; exec case calls
   `render_passes.geometry.masked_bind_group.recreate(&ctx)?`.
3. Build masked PBR pipeline in `textures.rs::finalize_gpu_textures` (NOT at setup — needs
   the live texture pool): Phase A also `clone_because_texture_pool_changed` the masked
   bind group + `relayout` the masked pool; Phase B/C add the masked PBR variant
   (`MaskedVariant{shader_id:PBR, base:Pbr, dynamic_alpha:None}`) via
   `masked_pipelines.build_descriptors`; Phase D/E fold via `insert`. (At setup the pool
   is empty → masked meshes fall back to plain/solid, which is fine.)
4. Render + routing: `Renderable` gets `geometry_masked_render_pipeline_key: Option<...>`;
   `collect_renderables` (renderable.rs:172) sets it when `materials.alpha_cutoff(key)
   .is_some()` AND `masked_pipelines.get(msaa, shader_id, cull)` is Some. In
   geometry/render_pass.rs, masked renderables `set_bind_group(0, masked_group0)` + their
   masked pipeline, then the existing uniform-meta draw (mesh.rs already supports the
   non-instanced uniform-meta path). Bind plain camera group 0 back for non-masked.
5. Custom arm (B3): add `alpha_wgsl: Option<String>` to `MaterialRegistration` (+ all
   constructors: editor, scene-loader, MCP) gated on `alpha_mode=Mask`; in
   `pipeline_scheduler/launch.rs` add a `LaunchSlot::Masked`/install arm that, when an
   opaque per-shader-id pipeline compiles for a MASK custom material, ALSO compiles the
   masked variant (DynamicAlphaShaderInfo from `generate_wgsl_struct`/`generate_wgsl_loader`
   /`generate_wgsl_texture_helpers` + `reg.alpha_wgsl`). Flip `materials.rs:135`
   `Material::Custom` to drop `Mask` from `is_transparency_pass`.
6. Editor 2nd-WGSL window (shown when alpha-mode=cutoff) + MCP tool to set alpha_wgsl.
7. B2 shadow masked variant (shadows/shader) — gate on `alpha_cutoff` present regardless
   of opaque/transparent routing.

(historical) NEXT ACTION (start here): build the `geometry_masked` module as ONE cohesive unit
(see MODULE STRUCTURE below) — cache key + enum/dispatch arm + masked WGSL template +
augmented group-0 bind group + lazy per-shader-id pipeline pool + render integration +
`Material::Custom` Mask→visibility routing — then layer the custom alpha-only arm
(`alpha_wgsl`) + editor 2nd-window + MCP. First browser test = a PROCEDURAL custom
cutout on a plane. No smaller standalone increment adds value (routing flip alone just
regresses custom cutouts to solid; an unused `alpha_wgsl` field is cross-crate churn).
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
  reference opaque-only contract types `OpaqueShadingInput` etc.). Builtin
  (PBR/Unlit/Toon) masked variants emit just that material's base-color-alpha load;
  custom emits the alpha-only fragment (B3).
- MODULE STRUCTURE (settled): build a SEPARATE module, NOT a field on
  `ShaderCacheKeyGeometry` (that key has a fixed-enumerated 9-leaf pool; masked is
  per-shader-id + runtime-registered → needs a lazy HashMap pool like opaque's
  `main`). Mirror `render_passes/material_opaque/`:
  * `render_passes/geometry_masked/` (or `geometry/masked/`) with:
    - `shader/cache_key.rs`: `ShaderCacheKeyGeometryMasked { texture_pool_arrays_len,
      texture_pool_samplers_len, msaa_sample_count, shader_id, base,
      dynamic_shader: Option<DynamicAlphaShaderInfo{shader_includes, struct_decl,
      loader_decl, alpha_wgsl}> }`. Add `ShaderCacheKeyRenderPass::GeometryMasked`
      arm (`shader_cache_key.rs`) + the source-dispatch arm (find the
      `ShaderCacheKeyRenderPass → into_source()` match in `shaders.rs`/`shaders/`).
    - `shader/masked_wgsl/{bind_groups,vertex,fragment}.wgsl` + `template.rs`:
      vertex = reuse the plain geometry vertex (it already forwards
      `material_mesh_meta_offset` as flat varying @location(5), + triangle_index +
      barycentric) so morph/skin still apply; fragment = read
      `material_mesh_metas[material_mesh_meta_offset/256u]` → attribute offsets/
      stride/uv_sets_index/material_offset → `texture_uv(...)` from the merged pool
      → builtin: load base_color α+cutoff (mirror `pbr_get_material` header at
      base_index+1 cutoff, +2 base_color_tex(5), +7..10 factor) & sample via
      `texture_pool_sample`; custom: `custom_alpha_dynamic(...)`; `if α<cutoff
      { discard; }`. bind_groups.wgsl = the AUGMENTED group 0 (camera+frame_globals
      already there for the vertex, + materials, material_mesh_metas, visibility_data
      merged pool, texture_transforms, pool_tex_*/pool_sampler_*). Groups 1/2/3
      reuse the plain geometry transforms/meta/animation layouts verbatim.
    - `bind_group.rs`: build the augmented group-0 bind group (reuse
      `TexturePoolDeps::new(ctx, Render)` for the pool layout + entries; pool buffer
      accessors per the plumbing map: `ctx.materials.gpu_buffer`,
      `ctx.meshes.meta.material_gpu_buffer()`,
      `ctx.meshes.visibility_geometry_data_gpu_buffer()`,
      `ctx.textures.texture_transforms_gpu_buffer`). recreate via
      `BindGroupRecreateContext`.
    - `pipeline.rs`: a lazy `HashMap<(msaa,mipmaps,shader_id), RenderPipelineKey>`
      pool with `get_masked_render_pipeline_key(shader_id)` + `insert_dynamic` +
      `clear_dynamic`, mirroring `MaterialOpaquePipelines`. Pipeline layout =
      [augmented_group0_bgl, transforms_bgl, meta_bgl(storage+uniform variants),
      animation_bgl]. Same 4 color targets + depth as the plain geometry pipeline
      (so masked meshes write the same visibility buffer); add per-cull-mode leaves.
  * Compile hook: when a masked material is needed, compile its masked pipeline via
    the SAME flow that compiles opaque per-shader-id pipelines — `register_material`
    / `prewarm_pipelines` / `ensure_scene_pipelines`. For builtin PBR masked, compile
    on first use like opaque first-party.
  * Render integration: in `geometry/render_pass.rs`, masked renderables bind the
    augmented group-0 (instead of the plain camera group-0) + their masked pipeline.
    Collect masked meshes via `material.alpha_mask().is_some()` AND a compiled masked
    variant exists; carry a `geometry_masked_render_pipeline_key` on `Renderable`
    (alongside `geometry_render_pipeline_key`). Until a mesh's masked variant is
    compiled, it falls back to the plain geometry pipeline (renders SOLID) — the
    regression-free incremental property.
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

KHR-EXTENSION IMPLICATIONS (analyzed this session — our change is consistent):
- `has_transmission()` (`materials/src/pbr.rs:416`) gates ONLY refractive
  KHR_materials_transmission (samples `opaque_tex` → must be transparent). Diffuse
  transmission (KHR_materials_diffuse_transmission) is a BRDF lobe (`brdf.wgsl`,
  included by opaque's SHADER_INCLUDES) → needs NO framebuffer → an alpha-masked
  diffuse-transmission surface is correctly OPAQUE/alpha-tested (matches Khronos
  sample-viewer; better than the old transparent path: casts cutout shadows + lands
  in opaque_tex, no inter-leaf blend artifacts).
- Canonical test asset: `media/.../DiffuseTransmissionPlant/glTF/DiffuseTransmissionPlant.gltf`
  — mat `leaves` = alphaMode=MASK (cutoff absent → glTF default 0.5) + diffuse_transmission
  + doubleSided. After step A it routes opaque but renders SOLID rectangles until B
  adds the raster cutout. Great real-world B test (Mask + diffuse-transmission + 2-sided).
- Routing matrix: Mask-only / Mask+diffuse-transmission / Mask+other-BRDF-lobes →
  OPAQUE (cutout in the masked raster, B). Blend / Mask+refractive-transmission /
  Mask+volume → TRANSPARENT (cutout in the transparent fragment — VERIFIED still
  discards: `material_transparent/.../fragment.wgsl:230` + `helpers/material_color_calc.wgsl:53,511`).
  The two cutout paths are mutually exclusive → no double-discard.
- B-impl consequences: (1) the masked raster discard is extension-AGNOSTIC — purely
  `base_color.a < cutoff` (glTF MASK def); diffuse-transmission/volume/clearcoat shade
  later in the opaque compute. (2) B2 shadow alpha-test must gate on
  `alpha_mask().is_some()` (cutoff present), INDEPENDENT of opaque/transparent routing
  — a Mask+refractive material is transparent-routed but must still cast a hole-shaped
  shadow (shadow pass rasterizes all cast_shadows meshes). (3) ensure MASK-with-absent-
  cutoff writes 0.5 into the material buffer (convert/material-write path).

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
