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

// instance_attrs.wgsl is already included via bind_groups.wgsl above (the
// `InstanceAttr` struct must be declared before binding 23 references it).

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

{% if multisampled_geometry %}
/*************** START msaa.wgsl ******************/
{% include "material_opaque_wgsl/helpers/msaa.wgsl" %}
/*************** END msaa.wgsl ******************/
{% endif %}

/*************** START material_shading.wgsl ******************/
{% include "material_opaque_wgsl/helpers/material_shading.wgsl" %}
/*************** END material_shading.wgsl ******************/

{% if shader_id.is_dynamic() %}
/*************** START dynamic-material wrapper ******************/
// Auto-generated per the registered material's layout. See
// docs/dynamic-materials/contract-opaque.md for the
// `OpaqueShadingInput` / `OpaqueShadingOutput` / `MaterialData`
// contract.
//
// The contract types are declared here (inline rather than in
// shared_wgsl/) because they exist exclusively for the wrapper —
// first-party materials read their inputs from the kernel directly.

// MaterialData struct — auto-generated from the registered layout.
{{ dynamic_struct_decl|safe }}

// MaterialData accessor — auto-generated to walk the layout's byte
// offsets, reading values out of `materials: array<u32>` (from
// shared_wgsl/material.wgsl). The wrapper calls this once per pixel
// using `input.material_offset` and stuffs the result into
// `input.material`.
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

{% if debug.any() %}
/*************** START debug.wgsl ******************/
{% include "material_opaque_wgsl/helpers/debug.wgsl" %}
/*************** END debug.wgsl ******************/
{% endif %}


@compute @workgroup_size(8, 8)
fn main(
    @builtin(workgroup_id) wg_id: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>
) {
    // Tile lookup — the material classify pass populated
    // `classify_buckets.tiles` with packed
    // `(tile_x, tile_y)` coords per `shader_id` bucket. Our
    // pipeline's specialized `shader_id` picks the matching offset
    // statically; `workgroup_id.x` is the bucket entry index;
    // `local_invocation_id.xy` is the 8×8 thread → pixel offset.
    // Templated bucket_offset lookup — the pipeline is specialized
    // for one shader_id, so the askama if-branch resolves to exactly
    // one entry at template-render time. Walks the same bucket_entries
    // list the classify-pass template walks.
    let bucket_offset =
    {%- for entry in bucket_entries -%}
        {%- if shader_id == entry.shader_id -%}
        classify_buckets.{{ entry.offset_field() }}
        {%- endif -%}
    {%- endfor -%}
    ;
    let tile = classify_buckets.tiles[bucket_offset + wg_id.x];
    let coords = vec2<i32>(i32(tile.x * 8u + lid.x), i32(tile.y * 8u + lid.y));
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
    let frame_globals = frame_globals_from_raw(frame_globals_raw);


    // early return if we only hit skybox / no geometry (for all samples if MSAA).
    //
    // Classify routes skybox-containing tiles into the PBR bucket; Unlit / Toon
    // pipelines also see the tile if any pixel uses their material, but for
    // their skybox pixels they must *not* write — PBR owns the skybox sample
    // so the output isn't double-written.
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
            {% if shader_id == MaterialShaderId::PBR %}
                // PBR pipeline owns skybox-only pixels.
                let color = sample_skybox(coords, screen_dims_f32, camera, skybox_tex, skybox_sampler);
                textureStore(opaque_tex, coords, color);
            {% else %}
                // Unlit / Toon / FlipBook / Custom pipelines: don't shade
                // skybox — PBR pipeline's dispatch over the same tile
                // handles it.
            {% endif %}
            return;
        }
    {% else %}
        if (triangle_index == U32_MAX) {
            {% if shader_id == MaterialShaderId::PBR %}
                let color = sample_skybox(coords, screen_dims_f32, camera, skybox_tex, skybox_sampler);
                textureStore(opaque_tex, coords, color);
            {% endif %}
            return;
        }
    {% endif %}

    // Special case: we've hit the skybox in our main sample (triangle_index is U32_MAX)
    // and yet at least one other MSAA sample hit geometry (any_sample_hit is true from above)
    // so we need to blend all samples properly with the skybox and per-sample shading.
    // Same ownership rule as above — only PBR writes the resolve.
    {% if multisampled_geometry %}
        if (triangle_index == U32_MAX) {
            {% if shader_id == MaterialShaderId::PBR %}
                let lights_info_sky = get_lights_info();
                let resolve_result = msaa_resolve_samples(camera, coords, screen_dims, screen_dims_f32, lights_info_sky);

                if (resolve_result.valid_samples > 0u) {
                    let final_color = resolve_result.color / f32(resolve_result.valid_samples);
                    let final_alpha = resolve_result.alpha / f32(resolve_result.valid_samples);
                    textureStore(opaque_tex, coords, vec4<f32>(final_color, final_alpha));
                } else {
                    textureStore(opaque_tex, coords, vec4<f32>(1.0, 0.0, 1.0, 1.0));
                }
            {% endif %}
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


    // Barycentric tex is RGBA16uint: RG = bary.xy as u16 fixed-point,
    // BA = instance_id (split u32 via join32). Unpack to f32 here; the
    // instance_id is consumed at the bottom of the function for per-instance
    // tint application.
    let barycentric_raw = textureLoad(barycentric_tex, coords, 0);
    let bary_xy = vec2<f32>(f32(barycentric_raw.x), f32(barycentric_raw.y)) / 65535.0;
    let barycentric = vec3<f32>(bary_xy.x, bary_xy.y, 1.0 - bary_xy.x - bary_xy.y);
    let main_instance_id = join32(barycentric_raw.z, barycentric_raw.w);

    let material_offset = material_mesh_meta.material_offset;
    let shader_id = material_load_shader_id(material_offset);

    // Per-pixel `shader_id` guard. The material classify pass already
    // scopes our dispatch to tiles containing our specialized
    // `shader_id`, so the guard rejects only pixels of a *different*
    // shader_id that share a mixed-material tile with ours.
    {% if shader_id == MaterialShaderId::PBR %}
        if (shader_id != SHADER_ID_PBR) { return; }
    {% else if shader_id == MaterialShaderId::UNLIT %}
        if (shader_id != SHADER_ID_UNLIT) { return; }
    {% else if shader_id == MaterialShaderId::TOON %}
        if (shader_id != SHADER_ID_TOON) { return; }
    {% else if shader_id == MaterialShaderId::FLIPBOOK %}
        if (shader_id != SHADER_ID_FLIPBOOK) { return; }
    {% else if shader_id.is_dynamic() %}
        if (shader_id != {{ shader_id.as_u32() }}u) { return; }
    {% endif %}

    let vertex_attribute_stride = material_mesh_meta.vertex_attribute_stride / 4; // 4 bytes per float
    let attribute_indices_offset = material_mesh_meta.vertex_attribute_indices_offset / 4;
    let attribute_data_offset = material_mesh_meta.vertex_attribute_data_offset / 4;
    let visibility_geometry_data_offset = material_mesh_meta.visibility_geometry_data_offset / 4;
    let uv_sets_index = material_mesh_meta.uv_sets_index;

    let base_triangle_index = attribute_indices_offset + (triangle_index * 3u);
    let triangle_indices = vec3<u32>(
        bitcast<u32>(visibility_data[base_triangle_index]),
        bitcast<u32>(visibility_data[base_triangle_index + 1]),
        bitcast<u32>(visibility_data[base_triangle_index + 2])
    );

    let standard_coordinates = get_standard_coordinates(coords, screen_dims);

    // Load world-space TBN directly from geometry pass output (already transformed with morphs/skins)
    let packed_nt = textureLoad(normal_tangent_tex, coords, 0);
    let tbn = unpack_normal_tangent(packed_nt);
    let world_normal = tbn.N;

    let lights_info = get_lights_info();

    // Compute material color and apply lighting based on shader type.
    // Each opaque pipeline is specialized for one `shader_id`; the
    // template emits only the matching material's shading path
    // (PBR / Unlit / Toon). The dropped runtime if/else used to live
    // here — the askama match below replaces it.
    var color: vec3<f32>;
    var base_alpha: f32;

    {% if shader_id == MaterialShaderId::UNLIT %}
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
    {% else if shader_id == MaterialShaderId::TOON %}
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
    {% else if shader_id == MaterialShaderId::PBR %}
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

        {% if use_mesh_light_slices %}
            color = apply_lighting_per_mesh(
                material_color,
                standard_coordinates.surface_to_camera,
                standard_coordinates.world_position,
                lights_info,
                (material_mesh_meta.receive_shadows & material_mesh_meta.shadow_receiver_gate),
                material_mesh_meta.light_slice_offset,
                material_mesh_meta.light_slice_count,
            );
        {% else %}
            color = apply_lighting(
                material_color,
                standard_coordinates.surface_to_camera,
                standard_coordinates.world_position,
                lights_info,
                (material_mesh_meta.receive_shadows & material_mesh_meta.shadow_receiver_gate),
            );
        {% endif %}
        base_alpha = material_color.base.a;
    {% else if shader_id == MaterialShaderId::FLIPBOOK %}
        // FlipBook: grid-uniform sprite-sheet, sampled per
        // `frame_globals.time + time_offset`. Tints by `material.tint`.
        let flipbook_material = flipbook_get_material(material_offset);
        var flipbook_sampled: vec4<f32> = vec4<f32>(1.0);
        if flipbook_material.atlas_tex_info.exists {
            let flipbook_uv_attr = texture_uv(
                attribute_data_offset,
                triangle_indices,
                barycentric,
                flipbook_material.atlas_tex_info,
                vertex_attribute_stride,
                uv_sets_index,
            );
            let flipbook_cell_uv = flipbook_compute_cell_uv(
                flipbook_material,
                flipbook_uv_attr,
                frame_globals.time,
            );
            // Mip-mode-aware sample. Even on the gradient template,
            // flipbook quads sample at the cell-UV (which jumps
            // discontinuously between cells, breaking hardware
            // derivative-driven mip selection); pass zero derivatives
            // so the grad path lands at mip 0.
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
        let flipbook_result = flipbook_finalize_color(
            flipbook_material,
            flipbook_sampled,
            frame_globals.time,
        );
        color = flipbook_result.rgb;
        base_alpha = flipbook_result.a;
    {% else if shader_id.is_dynamic() %}
        // Dynamic custom material — wrapped fragment lives above.
        let dyn_material = material_data_load(material_offset);
        let dyn_input = OpaqueShadingInput(
            coords,
            screen_dims,
            triangle_index,
            barycentric,
            main_instance_id,
            world_normal,
            standard_coordinates.world_position,
            standard_coordinates.surface_to_camera,
            material_offset,
            dyn_material,
        );
        let dyn_out = custom_shade_dynamic(dyn_input);
        color = dyn_out.color;
        base_alpha = dyn_out.alpha;
    {% endif %}


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

    // Apply per-instance tint (color × tint.rgb, alpha × tint.a × attr.alpha).
    if (main_instance_id != INSTANCE_ATTR_NONE) {
        let attr = instance_attrs[main_instance_id];
        let tint = unpack4x8unorm(attr.color_packed);
        color = color * tint.rgb;
        base_alpha = base_alpha * tint.a * attr.alpha;
    }

    // Write to output texture for non-edge pixel
    textureStore(opaque_tex, coords, vec4<f32>(color, base_alpha));
}

fn get_triangle_indices(attribute_indices_offset: u32, triangle_index: u32) -> vec3<u32> {
    let base = attribute_indices_offset + (triangle_index * 3u);
    return vec3<u32>(
        bitcast<u32>(visibility_data[base]),
        bitcast<u32>(visibility_data[base + 1u]),
        bitcast<u32>(visibility_data[base + 2u]),
    );
}
