# Custom-material attribute accessors (non-zero UV / COLOR sets) — #33

Status: **IMPLEMENTED + verified.** `material_uv(input, set)` /
`material_vertex_color(input, set)` are emitted in all three custom-fragment
kernels (opaque-compute + edge-resolve + transparent forward), native-tested
(naga-validated in every variant — see `material_opaque/shader/template.rs`),
documented in `docs/dynamic-materials/contract-{opaque,transparent}.md` (served
via MCP `get_material_contract`), and the per-set VALUE was browser-confirmed
(plan-doc tail). The out-of-range CLAMP (step 2 below) landed last — opaque +
edge now carry `uv_set_count`/`color_set_count` in `OpaqueShadingInput` and
guard against them (the transparent path already clamped via its templated
set switch). The notes below are the original design artifact, kept for context.
Branch: `mesh-authoring`. Companion to #27 (multi-UV import/pack infra — DONE).

## Goal

Let an author of a **custom (dynamic-WGSL) material** read an arbitrary vertex
attribute set in their fragment WGSL:

```wgsl
let uv1   = material_uv(1u);            // TEXCOORD_1, barycentric-interpolated
let col1  = material_vertex_color(1u);  // COLOR_1
```

Today a custom material can only reach set 0 (and only implicitly, via the
built-in texture path). PBR's per-texture `uv_index` already selects a non-zero
set for *texture sampling* (#27), but there is no author-facing accessor for the
raw interpolated attribute of a non-zero set.

## Why this is small-ish (the data already exists)

This is a **visibility-buffer** renderer: UV / COLOR sets are NOT interpolated
`@location` varyings. The fragment/compute kernel fetches them directly from the
packed per-vertex attribute buffer (`visibility_data`) using offsets carried in
`MaterialMeshMeta`:

- `material_mesh_meta.uv_sets_index`   — float offset of TEXCOORD_0 in the vertex stride
- `material_mesh_meta.uv_set_count`    — number of UV sets present
- `material_mesh_meta.color_sets_index`— float offset of COLOR_0
- `material_mesh_meta.color_set_count` — number of COLOR sets present
- `material_mesh_meta.vertex_attribute_stride`, `vertex_attribute_data_offset`

Built-ins already fetch an arbitrary UV set this way — see
`material_opaque/.../helpers/texture_uvs.wgsl`:

```
fn _texture_uv_per_vertex(attribute_data_offset, set_index, vertex_index, stride, uv_sets_index) -> vec2<f32> {
    let vertex_start = attribute_data_offset + vertex_index * stride;
    let uv_offset    = uv_sets_index + set_index * 2u;          // 2 floats per UV set
    let index        = vertex_start + uv_offset;
    return vec2(visibility_data[index], visibility_data[index + 1]);
}
fn texture_uv(...) { /* barycentric blend of the 3 triangle verts */ }
```

So `FragmentInputs(u32)` having a *single* `BIT_UV` / `BIT_VERTEX_COLOR` flag is
NOT a blocker — those flags gate "interpolate attributes at all", but the actual
per-set fetch is index-driven against the buffer. **No VS→FS varying plumbing
change is needed.** The gap is purely an author-facing accessor + clamping.

## The precedent to mirror

`texture_uvs.wgsl` already emits a **variant-agnostic** sampler specifically for
custom materials (`texture_pool_sample`, emitted unconditionally across mipmap
on/off + MSAA on/off + opaque/edge kernels) precisely because a custom fragment
is compiled into ALL variants and a helper emitted in only one variant fails to
resolve in the others. The new accessors MUST follow the same rule: emit them
unconditionally in every kernel a custom fragment is spliced into.

## Implementation steps

1. **Emit two accessors** wherever the custom fragment is invoked (opaque
   compute kernel + edge-resolve kernel + the transparent fragment path):
   ```wgsl
   fn material_uv(set_index: u32) -> vec2<f32> {
       if (set_index >= material_mesh_meta.uv_set_count) { return vec2<f32>(0.0); }
       // same barycentric fetch as texture_uv(), but set_index-driven
       ...
   }
   fn material_vertex_color(set_index: u32) -> vec4<f32> {
       if (set_index >= material_mesh_meta.color_set_count) { return vec4<f32>(1.0); }
       // colors pack 4 floats per set at color_sets_index
       ...
   }
   ```
   They need the kernel-local `attribute_data_offset`, `triangle_indices`,
   `barycentric`, and `vertex_attribute_stride` already in scope at the splice
   point (same values `texture_uv` receives). Confirm those names per kernel and
   either pass them or read them from `material_mesh_meta` where available.

2. **Clamp out-of-range** sets to a benign default (uv→`vec2(0)`,
   color→`vec4(1)`) so an author sampling a set the mesh lacks can't OOB-read
   `visibility_data`.

3. **Author opt-in**: reads require the attribute data present. `FragmentInputs`
   already has `UV` + `VERTEX_COLOR`; the dynamic-material `fragment_inputs`
   declaration (scene-loader `inputs_from_keys`) already maps `"uv"` /
   `"vertex_color"`. No new flags. (A future per-set opt-in is unnecessary —
   fetch is index-driven and clamped.)

4. **Docs**: add `material_uv` / `material_vertex_color` to the custom-material
   WGSL contract surfaced via MCP `get_material_contract` + the authoring docs.

## Verification plan

- **Native (completable now)**: render the custom-material templates for every
  variant and assert (a) both accessors are present and (b) the module compiles
  (naga parse) in each — mirrors `renderer/src/shader_completeness.rs`. This
  pins the historically-tricky "resolves in every variant" property.
- **GPU (browser, needs an asset)**: a glTF/primitive with TEXCOORD_1 (and/or
  COLOR_1) differing from set 0; author a custom material that visualizes
  `material_uv(1)` (e.g. as color) and screenshot that it differs from
  `material_uv(0)`. **No multi-UV test asset is currently in the repo** — this is
  the gating dependency for the visual confirm (state-2). Options: author a
  2-UV primitive in `awsm-meshgen` for tests, or add one to
  `awsm-renderer-assets`.

## Concrete implementation pointers (investigated 2026-06-14)

The earlier "needs the kernel locals in scope" worry is resolved — the context is
already bundled and handed to the author wrapper, so the accessors are plain
helpers, no `var<private>` promotion:

- **Splice point**: the opaque compute kernel
  (`material_opaque_wgsl/compute.wgsl`, ~L315-333) builds a dynamic shading-input
  struct from the per-invocation locals (`barycentric`, `triangle_indices`,
  `attribute_data_offset`, `vertex_attribute_stride`) and calls
  `custom_shade_dynamic(dyn_input)`. The edge-resolve kernel and the transparent
  fragment (`custom_shade_transparent_dynamic`) have the analogous wrappers.
- **Author context**: the author fragment already receives that input struct, so
  `material_uv(in, set)` / `material_vertex_color(in, set)` just need
  `(input_struct, set_index)` — the fetch math is the existing
  `_texture_uv_per_vertex` / `_vertex_color_per_vertex` (both already
  set-parameterized) + `material_mesh_meta.uv_set_count` / `color_set_count` for
  the clamp. Confirm the input struct carries (or add) `attribute_data_offset` +
  `vertex_attribute_stride` + the `uv_sets_index`/`color_sets_index` (or read the
  latter from `material_mesh_meta`).
- **Emission site**: `awsm_materials::registry::build_materials_wgsl[_filtered]`
  assembles the author fragment + the always-present custom helpers (the
  `texture_pool_sample` precedent). Emit the two accessors there so they exist in
  every variant the author compiles into.
- **Native test harness ALREADY EXISTS**:
  `material_opaque/shader/template.rs` has `transparent_dynamic_template_renders_valid_wgsl`
  + `empty_registry_emits_no_dynamic_wrapper` (render the dynamic template + naga-
  validate). Extend: render a dynamic material referencing `material_uv(in, 1u)` /
  `material_vertex_color(in, 1u)` and assert it validates in opaque-compute, edge,
  AND transparent variants. That makes the codegen layer state-1 (the historically
  tricky "resolves in every variant" property); only the GPU visual stays state-2.

## Why not landed autonomously this pass

Correct end-to-end delivery touches 3 kernel variants of shader codegen and
requires the GPU visual confirm above, which needs a multi-UV asset the repo
lacks. Committing the codegen without that confirm would be an unverified partial
(violates done-means-done). This spec is the concrete progress; execution wants
either the multi-UV asset added first or the user present for the visual confirm.
