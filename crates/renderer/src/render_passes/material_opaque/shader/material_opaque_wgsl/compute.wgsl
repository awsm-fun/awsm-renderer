/*************** START color_space.wgsl ******************/
{% include "shared_wgsl/color_space.wgsl" %}
/*************** END color_space.wgsl ******************/

/*************** START debug.wgsl ******************/
{% include "shared_wgsl/debug.wgsl" %}
/*************** END debug.wgsl ******************/

/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

/*************** START math.wgsl ******************/
{% include "shared_wgsl/math.wgsl" %}
/*************** END math.wgsl ******************/

/*************** START mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END mesh_meta.wgsl ******************/

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

{% if multisampled_geometry %}
/*************** START msaa.wgsl ******************/
{% include "material_opaque_wgsl/helpers/msaa.wgsl" %}
/*************** END msaa.wgsl ******************/
{% endif %}

/*************** START material_shading.wgsl ******************/
{% include "material_opaque_wgsl/helpers/material_shading.wgsl" %}
/*************** END material_shading.wgsl ******************/

{% if debug.any() %}
/*************** START debug.wgsl ******************/
{% include "material_opaque_wgsl/helpers/debug.wgsl" %}
/*************** END debug.wgsl ******************/
{% endif %}


@compute @workgroup_size(8, 8)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    let coords = vec2<i32>(gid.xy);
    let screen_dims = textureDimensions(opaque_tex);
    let screen_dims_i32 = vec2<i32>(i32(screen_dims.x), i32(screen_dims.y));
    let screen_dims_f32 = vec2<f32>(f32(screen_dims.x), f32(screen_dims.y));
    let pixel_center = vec2<f32>(f32(coords.x) + 0.5, f32(coords.y) + 0.5);

    // Bounds check
    if (coords.x >= screen_dims_i32.x || coords.y >= screen_dims_i32.y) {
        return;
    }

    let visibility_data_info = textureLoad(visibility_data_tex, coords, 0);

    let triangle_index = join32(visibility_data_info.x, visibility_data_info.y);
    let material_meta_offset = join32(visibility_data_info.z, visibility_data_info.w);


    let camera = camera_from_raw(camera_raw);


    // early return if we only hit skybox / no geometry (for all samples if MSAA)
    {% if multisampled_geometry %}
        // With MSAA, check if ANY sample hit geometry before early returning
        var any_sample_hit = false;
        for (var s = 0u; s < {{ msaa_sample_count }}u; s++) {
            var vis_check: vec4<u32>;
            switch(s) {
                case 0u: { vis_check = textureLoad(visibility_data_tex, coords, 0); }
                case 1u: { vis_check = textureLoad(visibility_data_tex, coords, 1); }
                case 2u: { vis_check = textureLoad(visibility_data_tex, coords, 2); }
                case 3u, default: { vis_check = textureLoad(visibility_data_tex, coords, 3); }
            }
            if (join32(vis_check.x, vis_check.y) != U32_MAX) {
                any_sample_hit = true;
                break;
            }
        }

        if (!any_sample_hit) {
            // All samples are skybox - just render skybox
            let color = sample_skybox(coords, screen_dims_f32, camera, skybox_tex, skybox_sampler);
            textureStore(opaque_tex, coords, color);
            return;
        }
    {% else %}
        if (triangle_index == U32_MAX) {
            let color = sample_skybox(coords, screen_dims_f32, camera, skybox_tex, skybox_sampler);
            textureStore(opaque_tex, coords, color);
            return;
        }
    {% endif %}

    // Special case: we've hit the skybox in our main sample (triangle_index is U32_MAX)
    // and yet at least one other MSAA sample hit geometry (any_sample_hit is true from above)
    // so we need to blend all samples properly with the skybox and per-sample shading
    {% if multisampled_geometry %}
        if (triangle_index == U32_MAX) {
            let lights_info_sky = get_lights_info();
            let resolve_result = msaa_resolve_samples(camera, coords, screen_dims, screen_dims_f32, lights_info_sky);

            if (resolve_result.valid_samples > 0u) {
                let final_color = resolve_result.color / f32(resolve_result.valid_samples);
                let final_alpha = resolve_result.alpha / f32(resolve_result.valid_samples);
                textureStore(opaque_tex, coords, vec4<f32>(final_color, final_alpha));
            } else {
                textureStore(opaque_tex, coords, vec4<f32>(1.0, 0.0, 1.0, 1.0));
            }
            return;
        }
    {% endif %}

    // If we've reached this point, the main sample hit geometry.
    let material_mesh_meta = material_mesh_metas[material_meta_offset / META_SIZE_IN_BYTES];

    // return early if the geometry hit is hud element (will be redrawn in transparency pass)
    if (material_mesh_meta.is_hud == 1u) {
        // this may bleed a little due to MSAA, but that's okay since huds are redrawn later
        return;
    }


    let barycentric_data = textureLoad(barycentric_tex, coords, 0);
    let barycentric = vec3<f32>(barycentric_data.x, barycentric_data.y, 1.0 - barycentric_data.x - barycentric_data.y);

    let material_offset = material_mesh_meta.material_offset;
    let shader_id = material_load_shader_id(material_offset);

    let vertex_attribute_stride = material_mesh_meta.vertex_attribute_stride / 4; // 4 bytes per float
    let attribute_indices_offset = material_mesh_meta.vertex_attribute_indices_offset / 4;
    let attribute_data_offset = material_mesh_meta.vertex_attribute_data_offset / 4;
    let visibility_geometry_data_offset = material_mesh_meta.visibility_geometry_data_offset / 4;
    let uv_sets_index = material_mesh_meta.uv_sets_index;

    let base_triangle_index = attribute_indices_offset + (triangle_index * 3u);
    let triangle_indices = vec3<u32>(
        attribute_indices[base_triangle_index],
        attribute_indices[base_triangle_index + 1],
        attribute_indices[base_triangle_index + 2]
    );

    let standard_coordinates = get_standard_coordinates(coords, screen_dims);

    // Load world-space TBN directly from geometry pass output (already transformed with morphs/skins)
    let packed_nt = textureLoad(normal_tangent_tex, coords, 0);
    let tbn = unpack_normal_tangent(packed_nt);
    let world_normal = tbn.N;

    let lights_info = get_lights_info();

    // Compute material color and apply lighting based on shader type
    var color: vec3<f32>;
    var base_alpha: f32;

    if (shader_id == SHADER_ID_UNLIT) {
        // Unlit material path
        let unlit_material = unlit_get_material(material_offset);
        {% match mipmap %}
            {% when MipmapMode::Gradient %}
                let bary_derivs = textureLoad(barycentric_derivatives_tex, coords, 0);
                let unlit_color = compute_unlit_material_color(
                    triangle_indices,
                    attribute_data_offset,
                    unlit_material,
                    barycentric,
                    vertex_attribute_stride,
                    uv_sets_index,
                    bary_derivs,
                    world_normal,
                    camera.view,
                );
            {% when MipmapMode::None %}
                let unlit_color = compute_unlit_material_color(
                    triangle_indices,
                    attribute_data_offset,
                    unlit_material,
                    barycentric,
                    vertex_attribute_stride,
                    uv_sets_index,
                );
        {% endmatch %}
        color = compute_unlit_output(unlit_color);
        base_alpha = unlit_color.base.a;
    } else if (shader_id == SHADER_ID_TOON) {
        // Toon material path — banded N·L + stepped Blinn-Phong + rim.
        // Reads world position from the standard coordinates the surrounding
        // code already computes; doesn't sample textures (v1).
        let toon_material = toon_get_material(material_offset);
        color = compute_toon_lit_color(
            toon_material,
            world_normal,
            standard_coordinates.surface_to_camera,
            standard_coordinates.world_position,
            lights_info,
        );
        base_alpha = toon_material.base_color_factor.a;
    } else {
        // PBR material path (default)
        let pbr_material = pbr_get_material(material_offset);

        {% match mipmap %}
            {% when MipmapMode::Gradient %}
                let bary_derivs = textureLoad(barycentric_derivatives_tex, coords, 0);
                let material_color = compute_material_color(
                    camera,
                    triangle_indices,
                    attribute_data_offset,
                    triangle_index,
                    pbr_material,
                    barycentric,
                    vertex_attribute_stride,
                    uv_sets_index,
                    tbn,
                    bary_derivs,
                );
            {% when MipmapMode::None %}
                let material_color = compute_material_color(
                    camera,
                    triangle_indices,
                    attribute_data_offset,
                    triangle_index,
                    pbr_material,
                    barycentric,
                    vertex_attribute_stride,
                    uv_sets_index,
                    tbn,
                );
        {% endmatch %}

        if(pbr_material.debug_bitmask != 0u) {
            color = pbr_debug_material_color(pbr_material, material_color);
            base_alpha = 1.0;
            textureStore(opaque_tex, coords, vec4<f32>(color, base_alpha));
            return;
        }

        color = apply_lighting(
            material_color,
            standard_coordinates.surface_to_camera,
            standard_coordinates.world_position,
            lights_info
        );
        base_alpha = material_color.base.a;

    }


    // MSAA edge detection and per-sample processing
    {% if multisampled_geometry && !debug.msaa_detect_edges %}
        let samples_to_process = msaa_sample_count_for_pixel(camera, coords, pixel_center, screen_dims_f32, world_normal, triangle_index);

        // If more than 1 sample to process, it's an edge pixel
        if (samples_to_process > 1u) {
            let resolve_result = msaa_resolve_samples(camera, coords, screen_dims, screen_dims_f32, lights_info);

            if (resolve_result.valid_samples > 0u) {
                let final_color = resolve_result.color / f32(resolve_result.valid_samples);
                let final_alpha = resolve_result.alpha / f32(resolve_result.valid_samples);
                textureStore(opaque_tex, coords, vec4<f32>(final_color, final_alpha));
                return;
            }
        }
    {% endif %}

    {% if debug.normals %}
        // Debug visualization: encode normal as color
        textureStore(opaque_tex, coords, vec4<f32>(debug_normals(world_normal), 1.0));
        return;
    {% endif %}

    // Write to output texture for non-edge pixel
    textureStore(opaque_tex, coords, vec4<f32>(color, base_alpha));
}

fn get_triangle_indices(attribute_indices_offset: u32, triangle_index: u32) -> vec3<u32> {
    let base = attribute_indices_offset + (triangle_index * 3u);
    return vec3<u32>(
        attribute_indices[base],
        attribute_indices[base + 1u],
        attribute_indices[base + 2u],
    );
}
