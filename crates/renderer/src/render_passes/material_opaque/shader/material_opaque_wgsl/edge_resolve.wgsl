// Per-shader-id MSAA edge-resolve compute shader.
//
// Indirect-dispatched over the shader_id's per-bucket sample-entry
// list (workgroup_size = 64). Each thread handles one packed
// (edge_pixel_id:24, sample_mask:8) entry: walks the set bits of
// sample_mask, shades each sample using this shader_id's specific
// shading code, sums the contributions, and writes a single
// vec4<f32>(color_sum, sample_count_as_float) to
// `accumulator[edge_pixel_id × 4 + slot_index]`. `slot_index` is
// read from edge_slot_map's 4-byte packed shader_id list.
//
// No atomics. Each (edge_pixel_id, slot_index) is owned by exactly
// one shader_id, so concurrent writes are race-free.
//
// See docs/plans/more-optimizations.md § Pass structure step 4.

/*************** START color_space.wgsl ******************/
{% include "shared_wgsl/color_space.wgsl" %}
/*************** END color_space.wgsl ******************/

/*************** START debug.wgsl ******************/
{% include "shared_wgsl/debug.wgsl" %}
/*************** END debug.wgsl ******************/

/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

/*************** START frame_globals.wgsl ******************/
{% include "shared_wgsl/frame_globals.wgsl" %}
/*************** END frame_globals.wgsl ******************/

/*************** START math.wgsl ******************/
{% include "shared_wgsl/math.wgsl" %}
/*************** END math.wgsl ******************/

/*************** START mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END mesh_meta.wgsl ******************/

// instance_attrs.wgsl is already included via bind_groups.wgsl above.

/*************** START textures.wgsl ******************/
{% include "shared_wgsl/textures.wgsl" %}
/*************** END textures.wgsl ******************/

/*************** START vertex_color.wgsl ******************/
{% include "shared_wgsl/vertex_color.wgsl" %}
/*************** END vertex_color.wgsl ******************/

/*************** START vertex_color_attrib.wgsl ******************/
{% include "material_opaque_wgsl/helpers/vertex_color_attrib.wgsl" %}
/*************** END vertex_color_attrib.wgsl ******************/

/*************** START transforms.wgsl ******************/
{% include "shared_wgsl/transforms.wgsl" %}
/*************** END transforms.wgsl ******************/

/*************** START lights.wgsl ******************/
{% include "shared_wgsl/lighting/lights.wgsl" %}
/*************** END lights.wgsl ******************/

/*************** START brdf.wgsl ******************/
{% include "shared_wgsl/lighting/brdf.wgsl" %}
/*************** END brdf.wgsl ******************/

/*************** START unlit.wgsl ******************/
{% include "shared_wgsl/lighting/unlit.wgsl" %}
/*************** END unlit.wgsl ******************/

/*************** START material.wgsl ******************/
{% include "shared_wgsl/material.wgsl" %}
/*************** END material.wgsl ******************/

/*************** START extras.wgsl ******************/
{% include "shared_wgsl/extras.wgsl" %}
/*************** END extras.wgsl ******************/

{% match mipmap %}
    {% when MipmapMode::Gradient %}
/*************** START mipmap.wgsl ******************/
{% include "material_opaque_wgsl/helpers/mipmap.wgsl" %}
/*************** END mipmap.wgsl ******************/
    {% when _ %}
{% endmatch %}

/*************** START texture_uvs.wgsl ******************/
{% include "material_opaque_wgsl/helpers/texture_uvs.wgsl" %}
/*************** END texture_uvs.wgsl ******************/

/*************** START standard.wgsl ******************/
{% include "material_opaque_wgsl/helpers/standard.wgsl" %}
/*************** END standard.wgsl ******************/

/*************** START material_color.wgsl ******************/
{% include "material_opaque_wgsl/helpers/material_color_calc.wgsl" %}
/*************** END material_color.wgsl ******************/

/*************** START positions.wgsl ******************/
{% include "material_opaque_wgsl/helpers/positions.wgsl" %}
/*************** END positions.wgsl ******************/

/*************** START skybox.wgsl ******************/
{% include "material_opaque_wgsl/helpers/skybox.wgsl" %}
/*************** END skybox.wgsl ******************/

/*************** START msaa.wgsl ******************/
{% include "material_opaque_wgsl/helpers/msaa.wgsl" %}
/*************** END msaa.wgsl ******************/

/*************** START material_shading.wgsl ******************/
{% include "material_opaque_wgsl/helpers/material_shading.wgsl" %}
/*************** END material_shading.wgsl ******************/

{% if shader_id.is_dynamic() %}
/*************** START dynamic-material wrapper ******************/
{{ dynamic_struct_decl|safe }}
{{ dynamic_loader_decl|safe }}

struct OpaqueShadingInput {
    coords: vec2<i32>,
    screen_dims: vec2<u32>,
    triangle_index: u32,
    barycentric: vec3<f32>,
    main_instance_id: u32,
    world_normal: vec3<f32>,
    world_position: vec3<f32>,
    surface_to_camera: vec3<f32>,
    material_offset: u32,
    material: MaterialData,
};
struct OpaqueShadingOutput {
    color: vec3<f32>,
    alpha: f32,
};

fn custom_shade_dynamic(input: OpaqueShadingInput) -> OpaqueShadingOutput {
{{ dynamic_wgsl_fragment|safe }}
}
/*************** END dynamic-material wrapper ******************/
{% endif %}

// Shade a single MSAA sample for this shader_id and return
// (color, alpha). Reads visibility/barycentric/normal textures at the
// given (coords, sample_index). Returns a sentinel zero on samples
// that don't belong to this shader_id (the caller's mask gate filters
// these out upstream, but we keep the bail for robustness).
fn shade_sample(
    coords: vec2<i32>,
    sample_index: u32,
    camera: Camera,
    screen_dims: vec2<u32>,
    screen_dims_f32: vec2<f32>,
    lights_info: LightsInfo,
) -> vec4<f32> {
    let textures = msaa_load_sample_textures(coords, sample_index);
    let tri_id = join32(textures.vis_data.x, textures.vis_data.y);
    let mat_meta_off = join32(textures.vis_data.z, textures.vis_data.w);

    // Skybox / no geometry — caller's mask should never put us here
    // for a non-skybox shader_id, but bail safely.
    if (tri_id == U32_MAX) {
        return vec4<f32>(0.0);
    }
    let sample_mesh_meta = material_mesh_metas[mat_meta_off / META_SIZE_IN_BYTES];
    if (sample_mesh_meta.is_hud == 1u) {
        return vec4<f32>(0.0);
    }

    let bary_xy = vec2<f32>(f32(textures.bary.x), f32(textures.bary.y)) / 65535.0;
    let sample_bary = vec3<f32>(bary_xy.x, bary_xy.y, 1.0 - bary_xy.x - bary_xy.y);
    let sample_instance_id = join32(textures.bary.z, textures.bary.w);

    let sample_tbn = unpack_normal_tangent(textures.normal_tangent);
    let sample_normal = sample_tbn.N;
    let standard_coordinates = get_standard_coordinates(coords, screen_dims);

    let sample_mat_offset = sample_mesh_meta.material_offset;
    let sample_stride = sample_mesh_meta.vertex_attribute_stride / 4;
    let sample_indices_off = sample_mesh_meta.vertex_attribute_indices_offset / 4;
    let sample_data_off = sample_mesh_meta.vertex_attribute_data_offset / 4;
    let sample_uv_sets_idx = sample_mesh_meta.uv_sets_index;

    let base_tri = sample_indices_off + (tri_id * 3u);
    let sample_tri_indices = vec3<u32>(
        bitcast<u32>(visibility_data[base_tri]),
        bitcast<u32>(visibility_data[base_tri + 1u]),
        bitcast<u32>(visibility_data[base_tri + 2u])
    );

    // Per-pixel shader_id guard. The classify pass already restricts
    // this dispatch to samples of our shader_id, but the templated
    // guard catches any registry drift between classify + this
    // pipeline.
    let sample_shader_id = material_load_shader_id(sample_mat_offset);
    {% if shader_id == MaterialShaderId::PBR %}
        if (sample_shader_id != SHADER_ID_PBR) { return vec4<f32>(0.0); }
    {% else if shader_id == MaterialShaderId::UNLIT %}
        if (sample_shader_id != SHADER_ID_UNLIT) { return vec4<f32>(0.0); }
    {% else if shader_id == MaterialShaderId::TOON %}
        if (sample_shader_id != SHADER_ID_TOON) { return vec4<f32>(0.0); }
    {% else if shader_id == MaterialShaderId::FLIPBOOK %}
        if (sample_shader_id != SHADER_ID_FLIPBOOK) { return vec4<f32>(0.0); }
    {% else if shader_id.is_dynamic() %}
        if (sample_shader_id != {{ shader_id.as_u32() }}u) { return vec4<f32>(0.0); }
    {% endif %}

    var color: vec3<f32>;
    var base_alpha: f32;

    {% if shader_id == MaterialShaderId::UNLIT %}
        let unlit_material = unlit_get_material(sample_mat_offset);
        {% match mipmap %}
            {% when MipmapMode::Gradient %}
                let unlit_color = compute_unlit_material_color(
                    sample_tri_indices,
                    sample_data_off,
                    unlit_material,
                    sample_bary,
                    sample_stride,
                    sample_uv_sets_idx,
                    textures.bary_derivs,
                    sample_normal,
                    camera.view,
                );
            {% when MipmapMode::None %}
                let unlit_color = compute_unlit_material_color(
                    sample_tri_indices,
                    sample_data_off,
                    unlit_material,
                    sample_bary,
                    sample_stride,
                    sample_uv_sets_idx,
                );
        {% endmatch %}
        color = compute_unlit_output(unlit_color);
        base_alpha = unlit_color.base.a;
    {% else if shader_id == MaterialShaderId::TOON %}
        let toon_material = toon_get_material(sample_mat_offset);
        color = compute_toon_lit_color(
            toon_material,
            sample_normal,
            standard_coordinates.surface_to_camera,
            standard_coordinates.world_position,
            lights_info,
        );
        base_alpha = toon_material.base_color_factor.a;
    {% else if shader_id == MaterialShaderId::PBR %}
        let pbr_material = pbr_get_material(sample_mat_offset);
        {% match mipmap %}
            {% when MipmapMode::Gradient %}
                let material_color = compute_material_color(
                    camera,
                    sample_tri_indices,
                    sample_data_off,
                    tri_id,
                    pbr_material,
                    sample_bary,
                    sample_stride,
                    sample_uv_sets_idx,
                    sample_tbn,
                    textures.bary_derivs,
                );
            {% when MipmapMode::None %}
                let material_color = compute_material_color(
                    camera,
                    sample_tri_indices,
                    sample_data_off,
                    tri_id,
                    pbr_material,
                    sample_bary,
                    sample_stride,
                    sample_uv_sets_idx,
                    sample_tbn,
                );
        {% endmatch %}
        {% if use_mesh_light_slices %}
            color = apply_lighting_per_mesh(
                material_color,
                standard_coordinates.surface_to_camera,
                standard_coordinates.world_position,
                lights_info,
                (sample_mesh_meta.receive_shadows & sample_mesh_meta.shadow_receiver_gate),
                sample_mesh_meta.light_slice_offset,
                sample_mesh_meta.light_slice_count,
            );
        {% else %}
            color = apply_lighting(
                material_color,
                standard_coordinates.surface_to_camera,
                standard_coordinates.world_position,
                lights_info,
                (sample_mesh_meta.receive_shadows & sample_mesh_meta.shadow_receiver_gate),
            );
        {% endif %}
        base_alpha = material_color.base.a;
    {% else if shader_id == MaterialShaderId::FLIPBOOK %}
        let flipbook_material = flipbook_get_material(sample_mat_offset);
        var flipbook_sampled: vec4<f32> = vec4<f32>(1.0);
        if flipbook_material.atlas_tex_info.exists {
            let flipbook_uv_attr = texture_uv(
                sample_data_off,
                sample_tri_indices,
                sample_bary,
                flipbook_material.atlas_tex_info,
                sample_stride,
                sample_uv_sets_idx,
            );
            let frame_globals_e = frame_globals_from_raw(frame_globals_raw);
            let flipbook_cell_uv = flipbook_compute_cell_uv(
                flipbook_material,
                flipbook_uv_attr,
                frame_globals_e.time,
            );
            {% match mipmap %}
                {% when MipmapMode::Gradient %}
                    let flipbook_uv_derivs = UvDerivs(vec2<f32>(0.0), vec2<f32>(0.0));
                    flipbook_sampled = texture_pool_sample_grad(
                        flipbook_material.atlas_tex_info,
                        flipbook_cell_uv,
                        flipbook_uv_derivs,
                    );
                {% when MipmapMode::None %}
                    flipbook_sampled = texture_pool_sample_no_mips(
                        flipbook_material.atlas_tex_info,
                        flipbook_cell_uv,
                    );
            {% endmatch %}
        }
        let frame_globals_e2 = frame_globals_from_raw(frame_globals_raw);
        let flipbook_result = flipbook_finalize_color(
            flipbook_material,
            flipbook_sampled,
            frame_globals_e2.time,
        );
        color = flipbook_result.rgb;
        base_alpha = flipbook_result.a;
    {% else if shader_id.is_dynamic() %}
        let dyn_material = material_data_load(sample_mat_offset);
        let dyn_input = OpaqueShadingInput(
            coords,
            screen_dims,
            tri_id,
            sample_bary,
            sample_instance_id,
            sample_normal,
            standard_coordinates.world_position,
            standard_coordinates.surface_to_camera,
            sample_mat_offset,
            dyn_material,
        );
        let dyn_out = custom_shade_dynamic(dyn_input);
        color = dyn_out.color;
        base_alpha = dyn_out.alpha;
    {% endif %}

    // Per-instance tint.
    if (sample_instance_id != INSTANCE_ATTR_NONE) {
        let attr = instance_attrs[sample_instance_id];
        let tint = unpack4x8unorm(attr.color_packed);
        color = color * tint.rgb;
        base_alpha = base_alpha * tint.a * attr.alpha;
    }

    return vec4<f32>(color, base_alpha);
}

@compute @workgroup_size(64)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
) {
    // Indirect dispatch sized so workgroup_count_x × 64 ≥ entry_count.
    // Each thread processes one packed (edge_pixel_id, sample_mask)
    // entry from this shader_id's sample list. The list lives at the
    // host-supplied `sample_list_base` offset in the storage buffer.
    let thread_index = gid.x;
    let entry_count = edge_buffers.{{ bucket_args_field }}_edge.workgroup_count_x;
    // workgroup_count_x in indirect args was atomicAdded-per-entry by
    // classify; the dispatch sized to ceil(count / 64) workgroups means
    // some threads overshoot. Bail.
    if (thread_index >= entry_count * 64u) {
        // Should never hit — count is already divided. Defensive bail.
        return;
    }
    if (thread_index >= edge_layout.sample_entries_per_bucket) {
        return;
    }
    let packed_entry = edge_buffers.data[edge_layout.{{ bucket_sample_list_base }} + thread_index];
    if (packed_entry == 0u) {
        // Empty entry sentinel.
        return;
    }
    let edge_pixel_id = packed_entry & 0x00FFFFFFu;
    let sample_mask = (packed_entry >> 24u) & 0xFFu;
    if (sample_mask == 0u) {
        return;
    }

    let packed_xy = edge_buffers.data[edge_layout.edge_to_xy_base + edge_pixel_id];
    let coords = vec2<i32>(
        i32(packed_xy & 0xFFFFu),
        i32((packed_xy >> 16u) & 0xFFFFu),
    );

    // Find our slot in the slot_map (4 bytes packed). The byte's
    // value is the bucket_index this thread's shader_id was assigned;
    // we know our own bucket_index statically via the template.
    let slot_map = edge_buffers.data[edge_layout.edge_slot_map_base + edge_pixel_id];
    var slot_index: u32 = 4u;
    for (var i = 0u; i < 4u; i++) {
        let byte_val = (slot_map >> (i * 8u)) & 0xFFu;
        if (byte_val == {{ bucket_index }}u) {
            slot_index = i;
            break;
        }
    }
    if (slot_index >= 4u) {
        return;
    }

    let camera = camera_from_raw(camera_raw);
    let screen_dims_u = textureDimensions(visibility_data_tex);
    let screen_dims = vec2<u32>(screen_dims_u.x, screen_dims_u.y);
    let screen_dims_f32 = vec2<f32>(f32(screen_dims.x), f32(screen_dims.y));
    let lights_info = get_lights_info();

    var color_sum = vec3<f32>(0.0);
    var alpha_sum: f32 = 0.0;
    var sample_count: u32 = 0u;

    for (var s = 0u; s < 4u; s++) {
        if ((sample_mask & (1u << s)) != 0u) {
            let shaded = shade_sample(coords, s, camera, screen_dims, screen_dims_f32, lights_info);
            color_sum += shaded.rgb;
            alpha_sum += shaded.a;
            sample_count += 1u;
        }
    }

    if (sample_count == 0u) {
        return;
    }

    // Write to accumulator[edge_pixel_id × 4 + slot_index]. The
    // accumulator is laid out as `array<vec4<f32>>` starting at
    // `accumulator_base` (in u32 strides; each vec4 takes 4 u32s).
    let accum_word_index = edge_layout.accumulator_base + (edge_pixel_id * 4u + slot_index) * 4u;
    edge_buffers.data[accum_word_index + 0u] = bitcast<u32>(color_sum.x);
    edge_buffers.data[accum_word_index + 1u] = bitcast<u32>(color_sum.y);
    edge_buffers.data[accum_word_index + 2u] = bitcast<u32>(color_sum.z);
    // Pack (alpha_sum, sample_count_as_float) into the w component —
    // final_blend needs both. We pack them into a vec2<f16>-ish encoding
    // since two values must share one slot; alpha_sum maps to the low
    // bits of bitcast<u32>(f32) is non-trivial, so use the .w slot for
    // sample_count (final blend recomputes alpha as alpha_sum / count
    // via a separate buffer if needed). Stage 3.7 may add a parallel
    // alpha buffer if alpha-resolve quality demands it.
    edge_buffers.data[accum_word_index + 3u] = bitcast<u32>(f32(sample_count));
}
