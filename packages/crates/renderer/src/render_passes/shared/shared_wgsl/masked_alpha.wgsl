// Shared masking-alpha helpers for the alpha-tested (MASK) raster variants.
//
// Used by BOTH the geometry masked fragment (visibility raster cutout) and the
// shadow masked fragment (hole-shaped shadow cutout). Provides the per-fragment
// masking alpha at a given barycentric — built-in PBR/Unlit/Toon read base-color
// alpha, Flipbook reads the CURRENT atlas cell's alpha (time-varying),
// alpha × factor; a dynamic (custom) material runs the author's alpha-only WGSL.
//
// The including template MUST define these context fields:
//   texture_pool_arrays_len, texture_pool_samplers_len: u32
//   base: ShadingBase
//   dynamic_struct_decl, dynamic_loader_decl, dynamic_texture_helpers,
//   dynamic_alpha_wgsl: String   (empty unless base == Custom)
//   flipbook_cell_wgsl: String   (empty unless base == Flipbook)
//
// and bind (on whichever group it augments): materials, material_mesh_metas,
// visibility_data, texture_transforms (storage) + the texture pool arrays/samplers.

{% include "shared_wgsl/math.wgsl" %}
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
{% include "shared_wgsl/textures.wgsl" %}

// ── Minimal material-buffer load helpers (the masked variant deliberately
// does NOT pull in shared_wgsl/material.wgsl, which would emit the full
// `materials_wgsl` blob — including dynamic-material color fragments that
// reference opaque-only contract types). ───────────────────────────────────
fn material_load_u32(index: u32) -> u32 { return bitcast<u32>(materials[index]); }
fn material_load_f32(index: u32) -> f32 { return bitcast<f32>(materials[index]); }
fn material_load_texture_info_raw(index: u32) -> TextureInfoRaw {
    return TextureInfoRaw(
        material_load_u32(index + 0u),
        material_load_u32(index + 1u),
        material_load_u32(index + 2u),
        material_load_u32(index + 3u),
        material_load_u32(index + 4u),
    );
}
fn material_load_texture_info(index: u32) -> TextureInfo {
    return convert_texture_info(material_load_texture_info_raw(index));
}

// ── UV reconstruction from the merged geometry pool (mirrors the opaque
// pass's texture_uvs.wgsl) ──────────────────────────────────────────────────
fn _mask_uv_per_vertex(attribute_data_offset: u32, set_index: u32, vertex_index: u32, vertex_attribute_stride: u32, uv_sets_index: u32) -> vec2<f32> {
    let vertex_start = attribute_data_offset + (vertex_index * vertex_attribute_stride);
    let uv_offset = uv_sets_index + (set_index * 2u);
    let index = vertex_start + uv_offset;
    return vec2<f32>(visibility_data[index], visibility_data[index + 1u]);
}
fn mask_texture_uv(attribute_data_offset: u32, triangle_indices: vec3<u32>, barycentric: vec3<f32>, tex_info: TextureInfo, vertex_attribute_stride: u32, uv_sets_index: u32) -> vec2<f32> {
    let uv0 = _mask_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, triangle_indices.x, vertex_attribute_stride, uv_sets_index);
    let uv1 = _mask_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, triangle_indices.y, vertex_attribute_stride, uv_sets_index);
    let uv2 = _mask_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, triangle_indices.z, vertex_attribute_stride, uv_sets_index);
    return barycentric.x * uv0 + barycentric.y * uv1 + barycentric.z * uv2;
}

// ── LOD-0 texture-pool sampler (compute/raster has no auto-derivatives for the
// visibility-pass discard; LOD 0 is the correct, cheap choice). ──────────────
fn _mask_pool_sample_lod0(info: TextureInfo, uv: vec2<f32>, array_index: u32, sampler_index: u32) -> vec4<f32> {
    var color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    switch array_index {
        {% for i in 0..texture_pool_arrays_len %}
        case {{ i }}u: {
            switch sampler_index {
                {% for j in 0..texture_pool_samplers_len %}
                case {{ j }}u: {
                    color = textureSampleLevel(pool_tex_{{ i }}, pool_sampler_{{ j }}, uv, i32(info.layer_index), 0);
                }
                {% endfor %}
                default: {}
            }
        }
        {% endfor %}
        default: {}
    }
    return color;
}
fn mask_texture_pool_sample(info: TextureInfo, attribute_uv: vec2<f32>) -> vec4<f32> {
    let uv = texture_transform_uvs(attribute_uv, info);
    return _mask_pool_sample_lod0(info, uv, info.array_index, info.sampler_index);
}

{% if base == ShadingBase::Custom %}
// ── Dynamic (custom) material: the author's *alpha-only* fragment. ───────────
// Auto-generated `MaterialData` struct + loader (same generators the opaque
// pass uses) so the author reads per-instance uniforms, and the per-texture
// `material_sample_<name>` helpers so a texture-based cutout can sample.
{{ dynamic_struct_decl|safe }}
{{ dynamic_loader_decl|safe }}
// The generated `material_sample_<name>` helpers call `texture_pool_sample`;
// alias it to the masked pass's LOD-0 sampler so they resolve (the opaque pass
// emits its own `texture_pool_sample` from texture_uvs.wgsl, which the masked
// variant deliberately does not include).
fn texture_pool_sample(info: TextureInfo, attribute_uv: vec2<f32>) -> vec4<f32> {
    return mask_texture_pool_sample(info, attribute_uv);
}
{{ dynamic_texture_helpers|safe }}

// Input handed to the author's alpha-only fragment. `uv` is the interpolated
// TEXCOORD_0 (convenience for procedural + the common single-UV case); the raw
// attribute accessors are forwarded for multi-UV / vertex-color reads.
struct MaskAlphaInput {
    uv: vec2<f32>,
    barycentric: vec3<f32>,
    triangle_indices: vec3<u32>,
    attribute_data_offset: u32,
    vertex_attribute_stride: u32,
    color_sets_index: u32,
    material_offset: u32,
    material: MaterialData,
};

// Wrapped author fragment — must `return` an `f32` alpha in [0,1].
fn custom_alpha_dynamic(input: MaskAlphaInput) -> f32 {
{{ dynamic_alpha_wgsl|safe }}
}
{% endif %}

// ── Masking alpha at a given barycentric. Re-evaluated per MSAA sample (at the
// sub-pixel sample positions) so the cutout is anti-aliased for ANY alpha —
// smooth (texture) OR binary (procedural `select`), where an analytic
// fwidth-of-alpha derivative would carry no sub-pixel information. Unified
// signature across bases (the Custom body loads its own `MaterialData`), so the
// per-sample call site is identical regardless of material type. ─────────────
{% if base == ShadingBase::Flipbook %}
// Shared sprite-sheet CELL math, injected verbatim from the materials crate
// (`awsm_materials::flipbook::FLIPBOOK_CELL_WGSL`) — the SAME functions the
// shaded material fragment runs, so the cutout can never disagree with the
// visible cell.
{{ flipbook_cell_wgsl }}
{% endif %}

{% if base == ShadingBase::Custom %}
fn mask_alpha_at(
    bary: vec3<f32>,
    triangle_indices: vec3<u32>,
    attribute_data_offset: u32,
    vertex_attribute_stride: u32,
    uv_sets_index: u32,
    color_sets_index: u32,
    material_offset: u32,
) -> f32 {
    let uv = _mask_uv_per_vertex(attribute_data_offset, 0u, triangle_indices.x, vertex_attribute_stride, uv_sets_index) * bary.x
        + _mask_uv_per_vertex(attribute_data_offset, 0u, triangle_indices.y, vertex_attribute_stride, uv_sets_index) * bary.y
        + _mask_uv_per_vertex(attribute_data_offset, 0u, triangle_indices.z, vertex_attribute_stride, uv_sets_index) * bary.z;
    let mat = material_data_load(material_offset);
    return custom_alpha_dynamic(MaskAlphaInput(
        uv,
        bary,
        triangle_indices,
        attribute_data_offset,
        vertex_attribute_stride,
        color_sets_index,
        material_offset,
        mat,
    ));
}
{% else if base == ShadingBase::Flipbook %}
fn mask_alpha_at(
    bary: vec3<f32>,
    triangle_indices: vec3<u32>,
    attribute_data_offset: u32,
    vertex_attribute_stride: u32,
    uv_sets_index: u32,
    color_sets_index: u32,
    material_offset: u32,
) -> f32 {
    // FlipBook atlas-cell alpha — the TIME-VARYING cutout. Word layout per
    // `FlipBookMaterial::write_uniform_buffer` (base_index = offset/4 + 1
    // skips the shader_id word): alpha_mode @+0, alpha_cutoff @+1,
    // atlas_tex @+2..6, tint @+7..10, cols @+11, rows @+12, frame_count
    // @+13, fps @+14, time_offset @+15, mode @+16, flip_y @+17. The CPU
    // writer clamps cols/rows/frame_count >= 1; the max() here is defense
    // against a stale/zeroed buffer, never a behavior change.
    let base_index = (material_offset / 4u) + 1u;
    let atlas_tex = material_load_texture_info(base_index + 2u);
    let tint_a = material_load_f32(base_index + 10u);
    let cols = max(material_load_u32(base_index + 11u), 1u);
    let rows = max(material_load_u32(base_index + 12u), 1u);
    let frame_count = max(material_load_u32(base_index + 13u), 1u);
    let fps = material_load_f32(base_index + 14u);
    let time_offset = material_load_f32(base_index + 15u);
    let mode = material_load_u32(base_index + 16u);
    let flip_y = material_load_u32(base_index + 17u);
    let frame_f = (frame_globals_raw.time + time_offset) * fps;
    // `Once` past the end: the quad is GONE — fully cut out.
    if flipbook_is_past_end(frame_f, frame_count, mode) {
        return 0.0;
    }
    var alpha = tint_a;
    if atlas_tex.exists {
        let in_uv = mask_texture_uv(attribute_data_offset, triangle_indices, bary, atlas_tex, vertex_attribute_stride, uv_sets_index);
        let cell_uv = flipbook_cell_uv(in_uv, frame_globals_raw.time, cols, rows, frame_count, fps, time_offset, mode, flip_y);
        alpha = alpha * mask_texture_pool_sample(atlas_tex, cell_uv).a;
    }
    return alpha;
}
{% else %}
fn mask_alpha_at(
    bary: vec3<f32>,
    triangle_indices: vec3<u32>,
    attribute_data_offset: u32,
    vertex_attribute_stride: u32,
    uv_sets_index: u32,
    color_sets_index: u32,
    material_offset: u32,
) -> f32 {
    // PBR base-color alpha. Header: word 0 = shader_id; base_index =
    // material_offset/4 + 1 (alpha_mode @+0, alpha_cutoff @+1,
    // base_color_tex @+2..6, base_color_factor @+7..10).
    let base_index = (material_offset / 4u) + 1u;
    let base_color_tex = material_load_texture_info(base_index + 2u);
    var alpha = material_load_f32(base_index + 10u);
    if base_color_tex.exists {
        let uv = mask_texture_uv(attribute_data_offset, triangle_indices, bary, base_color_tex, vertex_attribute_stride, uv_sets_index);
        alpha = alpha * mask_texture_pool_sample(base_color_tex, uv).a;
    }
    return alpha;
}
{% endif %}
