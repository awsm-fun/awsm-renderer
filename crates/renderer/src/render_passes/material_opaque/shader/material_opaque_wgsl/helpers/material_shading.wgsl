// Helper functions for material shading to reduce repetition in compute.wgsl

// Result from MSAA per-sample processing
struct MsaaResolveResult {
    color: vec3<f32>,
    alpha: f32,
    valid_samples: u32,
}

// Result from processing a single MSAA sample
struct MsaaSampleResult {
    color: vec3<f32>,
    alpha: f32,
    is_valid: bool,
}

// Texture data loaded for a single MSAA sample.
// `bary` carries the raw RGBA16uint texel for barycentric_tex: RG channels
// are u16 fixed-point barycentric, BA channels are the per-fragment
// instance_id (split via `join32` on read). Unpack to f32 / instance_id
// at the use sites.
struct MsaaSampleTextures {
    vis_data: vec4<u32>,
    bary: vec4<u32>,
    bary_derivs: vec4<f32>,
    normal_tangent: vec4<f32>,
}

{% if multisampled_geometry %}
// Apply per-instance tint to a sample's (color, alpha) and pack into the
// `MsaaSampleResult` carried back to the resolve loop. Identity passthrough
// when `instance_id == INSTANCE_ATTR_NONE` (i.e. non-instanced mesh).
fn msaa_apply_instance_tint(
    color: vec3<f32>,
    alpha: f32,
    instance_id: u32,
) -> MsaaSampleResult {
    if (instance_id == INSTANCE_ATTR_NONE) {
        return MsaaSampleResult(color, alpha, true);
    }
    let attr = instance_attrs[instance_id];
    let tint = unpack4x8unorm(attr.color_packed);
    return MsaaSampleResult(color * tint.rgb, alpha * tint.a * attr.alpha, true);
}

// Load texture data for a single MSAA sample
fn msaa_load_sample_textures(coords: vec2<i32>, sample_index: u32) -> MsaaSampleTextures {
    var result: MsaaSampleTextures;
    switch(sample_index) {
        case 0u: {
            result.vis_data = textureLoad(visibility_data_tex, coords, 0);
            result.bary = textureLoad(barycentric_tex, coords, 0);
            result.bary_derivs = textureLoad(barycentric_derivatives_tex, coords, 0);
            result.normal_tangent = textureLoad(normal_tangent_tex, coords, 0);
        }
        case 1u: {
            result.vis_data = textureLoad(visibility_data_tex, coords, 1);
            result.bary = textureLoad(barycentric_tex, coords, 1);
            result.bary_derivs = textureLoad(barycentric_derivatives_tex, coords, 1);
            result.normal_tangent = textureLoad(normal_tangent_tex, coords, 1);
        }
        case 2u: {
            result.vis_data = textureLoad(visibility_data_tex, coords, 2);
            result.bary = textureLoad(barycentric_tex, coords, 2);
            result.bary_derivs = textureLoad(barycentric_derivatives_tex, coords, 2);
            result.normal_tangent = textureLoad(normal_tangent_tex, coords, 2);
        }
        case 3u, default: {
            result.vis_data = textureLoad(visibility_data_tex, coords, 3);
            result.bary = textureLoad(barycentric_tex, coords, 3);
            result.bary_derivs = textureLoad(barycentric_derivatives_tex, coords, 3);
            result.normal_tangent = textureLoad(normal_tangent_tex, coords, 3);
        }
    }
    return result;
}

// Process a single MSAA sample - matches main branch logic closely
fn msaa_process_sample(
    camera: Camera,
    coords: vec2<i32>,
    screen_dims_f32: vec2<f32>,
    lights_info: LightsInfo,
    standard_coordinates: StandardCoordinates,
    textures: MsaaSampleTextures,
) -> MsaaSampleResult {
    let tri_id = join32(textures.vis_data.x, textures.vis_data.y);
    let mat_meta_off = join32(textures.vis_data.z, textures.vis_data.w);

    // Sample hit background - use skybox
    if (tri_id == U32_MAX) {
        let skybox_col = sample_skybox(coords, screen_dims_f32, camera, skybox_tex, skybox_sampler);
        return MsaaSampleResult(skybox_col.rgb, skybox_col.a, true);
    }

    let sample_mesh_meta = material_mesh_metas[mat_meta_off / META_SIZE_IN_BYTES];

    // Unpack barycentric from u16 fixed-point (no clamping - matches main)
    let bary_xy = vec2<f32>(f32(textures.bary.x), f32(textures.bary.y)) / 65535.0;
    let sample_bary = vec3<f32>(bary_xy.x, bary_xy.y, 1.0 - bary_xy.x - bary_xy.y);
    let sample_instance_id = join32(textures.bary.z, textures.bary.w);

    let sample_tbn = unpack_normal_tangent(textures.normal_tangent);
    let sample_normal = sample_tbn.N;

    // Extract mesh metadata
    let sample_mat_offset = sample_mesh_meta.material_offset;
    let sample_stride = sample_mesh_meta.vertex_attribute_stride / 4;
    let sample_indices_off = sample_mesh_meta.vertex_attribute_indices_offset / 4;
    let sample_data_off = sample_mesh_meta.vertex_attribute_data_offset / 4;
    let sample_vis_geom_off = sample_mesh_meta.visibility_geometry_data_offset / 4;
    let sample_uv_sets_idx = sample_mesh_meta.uv_sets_index;

    let base_tri = sample_indices_off + (tri_id * 3u);
    let sample_tri_indices = vec3<u32>(
        attribute_indices[base_tri],
        attribute_indices[base_tri + 1u],
        attribute_indices[base_tri + 2u]
    );

    // Check shader type and compute color accordingly
    let sample_shader_id = material_load_shader_id(sample_mat_offset);

    if (sample_shader_id == SHADER_ID_UNLIT) {
        let unlit_mat = unlit_get_material(sample_mat_offset);
        {% match mipmap %}
            {% when MipmapMode::Gradient %}
                let unlit_color = compute_unlit_material_color(
                    sample_tri_indices,
                    sample_data_off,
                    unlit_mat,
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
                    unlit_mat,
                    sample_bary,
                    sample_stride,
                    sample_uv_sets_idx,
                );
        {% endmatch %}
        return msaa_apply_instance_tint(
            compute_unlit_output(unlit_color),
            unlit_color.base.a,
            sample_instance_id,
        );
    } else if (sample_shader_id == SHADER_ID_TOON) {
        let toon_mat = toon_get_material(sample_mat_offset);
        let toon_color = compute_toon_lit_color(
            toon_mat,
            sample_normal,
            standard_coordinates.surface_to_camera,
            standard_coordinates.world_position,
            lights_info,
        );
        return msaa_apply_instance_tint(
            toon_color,
            toon_mat.base_color_factor.a,
            sample_instance_id,
        );
    } else {
        // PBR path
        let pbr_mat = pbr_get_material(sample_mat_offset);

        {% match mipmap %}
            {% when MipmapMode::Gradient %}
                let mat_color = compute_material_color(
                    camera,
                    sample_tri_indices,
                    sample_data_off,
                    tri_id,
                    pbr_mat,
                    sample_bary,
                    sample_stride,
                    sample_uv_sets_idx,
                    sample_tbn,
                    textures.bary_derivs,
                );
            {% when MipmapMode::None %}
                let mat_color = compute_material_color(
                    camera,
                    sample_tri_indices,
                    sample_data_off,
                    tri_id,
                    pbr_mat,
                    sample_bary,
                    sample_stride,
                    sample_uv_sets_idx,
                    sample_tbn,
                );
        {% endmatch %}

        if(pbr_mat.debug_bitmask != 0u) {
            let color = pbr_debug_material_color(pbr_mat, mat_color);
            // Debug viz bypasses tint so the raw debug color isn't masked.
            return MsaaSampleResult(color, mat_color.base.a, true);
        }

        // Use shared standard_coordinates like main branch does
        let color = apply_lighting(
            mat_color,
            standard_coordinates.surface_to_camera,
            standard_coordinates.world_position,
            lights_info,
            material_mesh_meta.receive_shadows,
        );
        return msaa_apply_instance_tint(color, mat_color.base.a, sample_instance_id);
    }
}

// Process all MSAA samples and blend their colors
fn msaa_resolve_samples(
    camera: Camera,
    coords: vec2<i32>,
    screen_dims: vec2<u32>,
    screen_dims_f32: vec2<f32>,
    lights_info: LightsInfo,
) -> MsaaResolveResult {
    // Use shared standard_coordinates for all samples (matches main branch)
    let standard_coordinates = get_standard_coordinates(coords, screen_dims);

    // Load and process each sample
    let textures_0 = msaa_load_sample_textures(coords, 0u);
    let textures_1 = msaa_load_sample_textures(coords, 1u);
    let textures_2 = msaa_load_sample_textures(coords, 2u);
    let textures_3 = msaa_load_sample_textures(coords, 3u);

    let result_0 = msaa_process_sample(camera, coords, screen_dims_f32, lights_info, standard_coordinates, textures_0);
    let result_1 = msaa_process_sample(camera, coords, screen_dims_f32, lights_info, standard_coordinates, textures_1);
    let result_2 = msaa_process_sample(camera, coords, screen_dims_f32, lights_info, standard_coordinates, textures_2);
    let result_3 = msaa_process_sample(camera, coords, screen_dims_f32, lights_info, standard_coordinates, textures_3);

    // Accumulate results
    var color_sum = vec3<f32>(0.0);
    var alpha_sum = 0.0;
    var valid_samples = 0u;

    if (result_0.is_valid) { color_sum += result_0.color; alpha_sum += result_0.alpha; valid_samples++; }
    if (result_1.is_valid) { color_sum += result_1.color; alpha_sum += result_1.alpha; valid_samples++; }
    if (result_2.is_valid) { color_sum += result_2.color; alpha_sum += result_2.alpha; valid_samples++; }
    if (result_3.is_valid) { color_sum += result_3.color; alpha_sum += result_3.alpha; valid_samples++; }

    return MsaaResolveResult(color_sum, alpha_sum, valid_samples);
}
{% endif %}

{% match mipmap %}
    {% when MipmapMode::Gradient %}
        // Compute material color with gradient-based mipmapping
        fn compute_material_color(
            camera: Camera,
            triangle_indices: vec3<u32>,
            attribute_data_offset: u32,
            triangle_index: u32,
            pbr_material: PbrMaterial,
            barycentric: vec3<f32>,
            vertex_attribute_stride: u32,
            uv_sets_index: u32,
            geometry_tbn: TBN,
            bary_derivs: vec4<f32>,
        ) -> PbrMaterialColor {
            let gradients = pbr_get_gradients(
                barycentric,
                bary_derivs,
                pbr_material,
                triangle_indices,
                attribute_data_offset,
                vertex_attribute_stride,
                uv_sets_index,
                geometry_tbn.N,
                camera.view
            );

            return pbr_get_material_color_grad(
                triangle_indices,
                attribute_data_offset,
                triangle_index,
                pbr_material,
                barycentric,
                vertex_attribute_stride,
                uv_sets_index,
                gradients,
                geometry_tbn,
            );
        }
    {% when MipmapMode::None %}
        // Compute material color without mipmapping
        fn compute_material_color(
            camera: Camera,
            triangle_indices: vec3<u32>,
            attribute_data_offset: u32,
            triangle_index: u32,
            pbr_material: PbrMaterial,
            barycentric: vec3<f32>,
            vertex_attribute_stride: u32,
            uv_sets_index: u32,
            geometry_tbn: TBN,
        ) -> PbrMaterialColor {
            return pbr_get_material_color_no_mips(
                triangle_indices,
                attribute_data_offset,
                triangle_index,
                pbr_material,
                barycentric,
                vertex_attribute_stride,
                uv_sets_index,
                geometry_tbn,
            );
        }
{% endmatch %}
