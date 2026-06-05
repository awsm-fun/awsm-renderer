// compute.wgsl — the opaque MATERIAL kernel (skybox-free; the canonical skybox
// bucket uses skybox_primary.wgsl instead). Shared preamble is factored out.
{% include "material_opaque_wgsl/opaque_kernel_includes.wgsl" %}


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
    // This is the pure material kernel — it never writes the skybox. The
    // dedicated skybox_primary.wgsl pipeline (compiled for the canonical skybox
    // bucket) owns skybox/uncovered pixels; every material pipeline just skips
    // them here so the output isn't double-written.
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
            // Skybox / fully-uncovered tile — the dedicated skybox_primary
            // pipeline writes these pixels; the material kernel just skips them.
            return;
        }
    {% else %}
        if (triangle_index == U32_MAX) {
            // Skybox pixel — handled by skybox_primary; skip.
            return;
        }
    {% endif %}

    // Sample 0 (the primary sample) is skybox but other samples hit
    // geometry — a silhouette edge pixel. This pure material kernel writes
    // nothing for it: skybox_primary owns the skybox contribution and
    // Stage 3 edge_resolve / final_blend own the per-sample blend, so the
    // kernel just skips the pixel (below) to avoid double-writing.
    {% if multisampled_geometry %}
        if (triangle_index == U32_MAX) {
            // Sample-0 skybox at a silhouette edge — skybox_primary writes the
            // base color; the material kernel skips here.
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
    // shader_id that share a mixed-material tile with ours. The guard
    // is on the numeric (registry-allocated) id regardless of `base`:
    // a specialized PBR variant routes only its own id's pixels here.
    if (shader_id != {{ shader_id.as_u32() }}u) { return; }

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

    {% if base == ShadingBase::Unlit %}
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
    {% else if base == ShadingBase::Toon %}
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
    {% else if base == ShadingBase::Pbr %}
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

        {% if use_froxel_lights %}
            // Unified froxel path: every opaque mesh shades punctual
            // lights from its per-pixel froxel light list (the GPU light
            // cull). This replaces the old per-mesh-slice / oversized-
            // sentinel split — clustered (froxel) culling is generic and
            // camera-correct for any mesh size, so there's no gate to
            // tune. Directional lights are walked flat (see lights.wgsl).
            color = apply_lighting_per_froxel(
                material_color,
                standard_coordinates.surface_to_camera,
                standard_coordinates.world_position,
                lights_info,
                (material_mesh_meta.receive_shadows & material_mesh_meta.shadow_receiver_gate),
                vec2<f32>(f32(coords.x), f32(coords.y)),
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
    {% else if base == ShadingBase::Flipbook %}
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
    {% else if base == ShadingBase::Custom %}
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


    // Edge-resolve is owned by the Stage 3 dispatch chain
    // (classify → per-shader edge_resolve → final_blend). Primary
    // opaque always writes the sample-0 shaded color here; final_blend
    // overwrites at classify-detected edge pixels with the proper
    // 4-sample average. This keeps the primary-opaque SPIR-V scoped
    // to a single shader_id (the per-pipeline specialization) — no
    // cross-shader switch inlined, no growth as dynamic materials
    // register. See https://github.com/dakom/awsm-renderer/pull/99 § Priority 3.

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

    {% if debug.views %}
    // Global wireframe view — replace the shaded surface with a uniform clay
    // fill and draw the triangle edges on top, so meshes read as a wireframe
    // regardless of their material (not edges tinted onto the lit result).
    // Constant barycentric threshold — derivatives aren't available in a
    // compute kernel.
    if (cull_params.debug_wireframe == 1u) {
        let wire_edge = min(min(barycentric.x, barycentric.y), barycentric.z);
        let wire = 1.0 - smoothstep(0.0, 0.02, wire_edge);
        color = mix(vec3<f32>(0.55, 0.57, 0.60), vec3<f32>(0.05, 0.05, 0.07), wire);
    }
    {% endif %}

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
