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

// Barycentric at a sub-pixel offset (pixel-space, center origin), via the
// screen-space derivatives of the interpolated barycentric.
fn mask_bary_at(b: vec2<f32>, dbx: vec2<f32>, dby: vec2<f32>, ox: f32, oy: f32) -> vec3<f32> {
    let p = b + dbx * ox + dby * oy;
    return vec3<f32>(p.x, p.y, 1.0 - p.x - p.y);
}

// ── Fragment I/O (identical to the plain geometry pass — masked meshes write
// the SAME visibility buffer; we only add the cutoff discard up front). ──────
struct FragmentInput {
    @location(0) @interpolate(flat) triangle_index: u32,
    @location(1) barycentric: vec2<f32>,
    @location(2) world_normal: vec3<f32>,
    @location(3) world_tangent: vec4<f32>,
    @location(4) @interpolate(flat) instance_id: u32,
    @location(5) @interpolate(flat) material_mesh_meta_offset: u32,
}

struct FragmentOutput {
    @location(0) visibility_data_out: vec4<u32>,
    @location(1) barycentric: vec4<u32>,
    @location(2) normal_tangent: vec4<f32>,
    @location(3) barycentric_derivatives: vec4<f32>,
    {% if msaa_sample_count > 1 %}
    // Per-sample cutout coverage. Fractional at the cutout boundary so the
    // covered samples write the surface while the rest stay the cleared skybox
    // sentinel — the MSAA edge-resolve then blends them (anti-aliased cutout).
    @builtin(sample_mask) coverage_mask: u32,
    {% endif %}
}

@fragment
fn fs_main(input: FragmentInput) -> FragmentOutput {
    let mm = material_mesh_metas[input.material_mesh_meta_offset / META_SIZE_IN_BYTES];
    let material_offset = mm.material_offset;
    let vertex_attribute_stride = mm.vertex_attribute_stride / 4u;
    let attribute_indices_offset = mm.vertex_attribute_indices_offset / 4u;
    let attribute_data_offset = mm.vertex_attribute_data_offset / 4u;
    let uv_sets_index = mm.uv_sets_index;
    let color_sets_index = mm.color_sets_index;

    let base_triangle_index = attribute_indices_offset + (input.triangle_index * 3u);
    let triangle_indices = vec3<u32>(
        bitcast<u32>(visibility_data[base_triangle_index]),
        bitcast<u32>(visibility_data[base_triangle_index + 1u]),
        bitcast<u32>(visibility_data[base_triangle_index + 2u]),
    );
    // ── Cutout: keep/discard the pixel, and (under MSAA) per-sample coverage ──
    {% if msaa_sample_count > 1 %}
    // A single center alpha test is enough to KEEP/DROP the pixel — but it's
    // all-or-nothing, so the cutout edge aliases. To ANTI-ALIAS it we need the
    // sub-pixel coverage: evaluate the masking alpha at each of the 4 MSAA
    // sample sub-positions (offset from pixel center via the barycentric
    // screen-space derivatives) and set that sample's coverage bit when it
    // passes the cutoff. The covered samples write the surface; the rest stay
    // the cleared background, and the existing MSAA edge-resolve blends them.
    // True per-sample sampling works for ANY alpha — smooth (texture) OR binary
    // (procedural `select`), where an fwidth-of-alpha gradient carries nothing.
    // Standard 4x sample offsets (the exact positions only nudge spatial
    // placement; the resolve blends by covered-sample COUNT, all carrying the
    // shared center data).
    let dbx = dpdx(input.barycentric);
    let dby = dpdy(input.barycentric);
    let cut = mm.alpha_cutoff;
    var coverage_mask = 0u;
    if mask_alpha_at(mask_bary_at(input.barycentric, dbx, dby, -0.125, -0.375), triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset) >= cut { coverage_mask |= 1u; }
    if mask_alpha_at(mask_bary_at(input.barycentric, dbx, dby,  0.375, -0.125), triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset) >= cut { coverage_mask |= 2u; }
    if mask_alpha_at(mask_bary_at(input.barycentric, dbx, dby, -0.375,  0.125), triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset) >= cut { coverage_mask |= 4u; }
    if mask_alpha_at(mask_bary_at(input.barycentric, dbx, dby,  0.125,  0.375), triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset) >= cut { coverage_mask |= 8u; }
    if coverage_mask == 0u {
        discard;
    }
    {% else %}
    let bary = vec3<f32>(input.barycentric.x, input.barycentric.y, 1.0 - input.barycentric.x - input.barycentric.y);
    let alpha = mask_alpha_at(bary, triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset);
    if alpha < mm.alpha_cutoff {
        discard;
    }
    {% endif %}

    // ── Write the visibility buffer exactly like the plain geometry pass ──
    var out: FragmentOutput;
    {% if msaa_sample_count > 1 %}
    out.coverage_mask = coverage_mask;
    {% endif %}
    let t = split16(input.triangle_index);
    let m = split16(input.material_mesh_meta_offset);
    out.visibility_data_out = vec4<u32>(t.x, t.y, m.x, m.y);

    let bary_xy = clamp(input.barycentric, vec2<f32>(0.0), vec2<f32>(1.0));
    let bary_x_u16 = u32(bary_xy.x * 65535.0 + 0.5);
    let bary_y_u16 = u32(bary_xy.y * 65535.0 + 0.5);
    let iid = split16(input.instance_id);
    out.barycentric = vec4<u32>(bary_x_u16, bary_y_u16, iid.x, iid.y);

    let N = normalize(input.world_normal);
    let T = normalize(input.world_tangent.xyz);
    let s = input.world_tangent.w;
    out.normal_tangent = pack_normal_tangent(N, T, s);

    let ddx = dpdx(input.barycentric);
    let ddy = dpdy(input.barycentric);
    out.barycentric_derivatives = vec4<f32>(ddx.x, ddy.x, ddx.y, ddy.y);

    return out;
}
