// compute.wgsl — the opaque MATERIAL kernel (skybox-free; the canonical skybox
// bucket uses skybox_primary.wgsl instead). Shared preamble is factored out.
{% include "material_opaque_wgsl/opaque_kernel_includes.wgsl" %}


@compute @workgroup_size(8, 8)
fn cs_opaque(
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
    let color_sets_index = material_mesh_meta.color_sets_index;
    let uv_set_count = material_mesh_meta.uv_set_count;
    let color_set_count = material_mesh_meta.color_set_count;

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

    {% if inc.light_access %}
    let lights_info = get_lights_info();
    {% endif %}

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
                    color_sets_index,
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
                    color_sets_index,
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
            triangle_indices,
            attribute_data_offset,
            vertex_attribute_stride,
            color_sets_index,
            uv_sets_index,
            color_set_count,
            uv_set_count,
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

{% if multisampled_geometry %}
// ════════════════════════════════════════════════════════════════════
// UNIFIED MODULE — `cs_edge` entry point (§ Part B, the "1024 fix").
//
// The per-shader-id MSAA edge-resolve kernel, merged into THIS module so
// each material compiles ONE shader module with TWO `@compute` entry
// points (`cs_opaque` above + `cs_edge` below) instead of two separate
// modules. The body below is the former `edge_resolve.wgsl` `main`,
// renamed to `cs_edge`; it shares the embedded per-material shading +
// the dynamic-material wrapper + all helper includes already pulled in
// by the shared preamble above (each global/binding/fn appears exactly
// once across both entry points). The `edge_data` / `edge_layout`
// bindings it reads are declared (gated on `multisampled_geometry`) in
// bind_groups.wgsl at group(3) bindings 10/11.
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
// See https://github.com/dakom/awsm-renderer/pull/99 § Pass structure step 4.
// ════════════════════════════════════════════════════════════════════

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
    {% if inc.light_access %}
    lights_info: LightsInfo,
    {% endif %}
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
    // NOTE: temporarily back on sample-0 depth (main-branch behaviour).
    // The per-sample variant produced dark per-sample shading deltas at
    // tessellated-curve silhouettes that, once averaged, looked like
    // wireframe artifacts at every intra-mesh triangle seam classify
    // detects as an edge.
    let standard_coordinates = get_standard_coordinates(coords, screen_dims);

    let sample_mat_offset = sample_mesh_meta.material_offset;
    let sample_stride = sample_mesh_meta.vertex_attribute_stride / 4;
    let sample_indices_off = sample_mesh_meta.vertex_attribute_indices_offset / 4;
    let sample_data_off = sample_mesh_meta.vertex_attribute_data_offset / 4;
    let sample_uv_sets_idx = sample_mesh_meta.uv_sets_index;
    let sample_color_sets_idx = sample_mesh_meta.color_sets_index;
    let sample_uv_set_count = sample_mesh_meta.uv_set_count;
    let sample_color_set_count = sample_mesh_meta.color_set_count;

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
    // Guard on the numeric (registry-allocated) id regardless of `base`.
    if (sample_shader_id != {{ shader_id.as_u32() }}u) { return vec4<f32>(0.0); }

    var color: vec3<f32>;
    var base_alpha: f32;

    {% if base == ShadingBase::Unlit %}
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
    {% else if base == ShadingBase::Toon %}
        let toon_material = toon_get_material(sample_mat_offset);
        color = compute_toon_lit_color(
            toon_material,
            sample_normal,
            standard_coordinates.surface_to_camera,
            standard_coordinates.world_position,
            lights_info,
        );
        base_alpha = toon_material.base_color_factor.a;
    {% else if base == ShadingBase::Pbr %}
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
                    sample_color_sets_idx,
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
                    sample_color_sets_idx,
                    sample_tbn,
                );
        {% endmatch %}
        {% if use_froxel_lights %}
            // Unified froxel path (mirrors the main compute pass): every
            // edge sample shades punctual lights from its per-pixel froxel
            // list. No per-mesh-slice / oversized-sentinel split.
            color = apply_lighting_per_froxel(
                material_color,
                standard_coordinates.surface_to_camera,
                standard_coordinates.world_position,
                lights_info,
                (sample_mesh_meta.receive_shadows & sample_mesh_meta.shadow_receiver_gate),
                vec2<f32>(f32(coords.x), f32(coords.y)),
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
    {% else if base == ShadingBase::Flipbook %}
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
    {% else if base == ShadingBase::Custom %}
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
            sample_tri_indices,
            sample_data_off,
            sample_stride,
            sample_color_sets_idx,
            sample_uv_sets_idx,
            sample_color_set_count,
            sample_uv_set_count,
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

    {% if debug.views %}
    // Global wireframe view — mirror the compute kernel (uses this pass's
    // per-sample barycentric): uniform clay fill + dark triangle edges, so the
    // surface reads as a wireframe rather than edges over the shaded material.
    if (cull_params.debug_wireframe == 1u) {
        let wire_edge = min(min(sample_bary.x, sample_bary.y), sample_bary.z);
        let wire = 1.0 - smoothstep(0.0, 0.02, wire_edge);
        color = mix(vec3<f32>(0.55, 0.57, 0.60), vec3<f32>(0.05, 0.05, 0.07), wire);
    }
    {% endif %}

    return vec4<f32>(color, base_alpha);
}

@compute @workgroup_size(64)
fn cs_edge(
    @builtin(global_invocation_id) gid: vec3<u32>,
) {
    // Indirect dispatch sized so workgroup_count_x × 64 ≥ entry_count.
    // Each thread processes one packed (edge_pixel_id, sample_mask)
    // entry from this shader_id's sample list. The list lives at the
    // host-supplied `sample_list_base` offset in the storage buffer.
    let thread_index = gid.x;
    // Per-shader entry count is mirrored into the data_buffer's header
    // (classify atomicAdds it alongside the args_buffer counter). Read
    // through the existing `edge_data` binding — no separate args
    // binding needed.
    let entry_count = edge_data[edge_layout.per_shader_count_base + {{ bucket_index }}u];
    if (thread_index >= entry_count) {
        return;
    }
    if (thread_index >= edge_layout.sample_entries_per_bucket) {
        return;
    }
    // This bucket's sample list base = base0 + bucket_index*stride (§4c).
    let bucket_list_base = edge_layout.per_shader_sample_list_base + {{ bucket_index }}u * edge_layout.sample_entries_per_bucket;
    let packed_entry = edge_data[bucket_list_base + thread_index];
    if (packed_entry == 0u) {
        // Empty entry sentinel.
        return;
    }
    let edge_pixel_id = packed_entry & 0x00FFFFFFu;
    let sample_mask = (packed_entry >> 24u) & 0xFFu;
    if (sample_mask == 0u) {
        return;
    }

    let packed_xy = edge_data[edge_layout.edge_to_xy_base + edge_pixel_id];
    let coords = vec2<i32>(
        i32(packed_xy & 0xFFFFu),
        i32((packed_xy >> 16u) & 0xFFFFu),
    );

    // Find our slot in the slot_map. Each of the 4 per-sample fields holds
    // the bucket_index that sample was assigned; we know our own bucket_index
    // statically via the template. §5: 8-bit packs 4×8 into one u32/edge,
    // 16-bit packs 4×16 into two u32/edge (lets the index exceed 254).
    {% if edge_slot_bits == 16 %}
    let slot_w0 = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id * 2u];
    let slot_w1 = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id * 2u + 1u];
    {% else %}
    let slot_map = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id];
    {% endif %}
    var slot_index: u32 = 4u;
    for (var i = 0u; i < 4u; i++) {
        {% if edge_slot_bits == 16 %}
        let word = select(slot_w0, slot_w1, i >= 2u);
        let field = (word >> ((i % 2u) * 16u)) & 0xFFFFu;
        {% else %}
        let field = (slot_map >> (i * 8u)) & 0xFFu;
        {% endif %}
        if (field == {{ bucket_index }}u) {
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
    {% if inc.light_access %}
    let lights_info = get_lights_info();
    {% endif %}

    var color_sum = vec3<f32>(0.0);
    var alpha_sum: f32 = 0.0;
    var sample_count: u32 = 0u;

    for (var s = 0u; s < 4u; s++) {
        if ((sample_mask & (1u << s)) != 0u) {
            let shaded = shade_sample(coords, s, camera, screen_dims, screen_dims_f32{% if inc.light_access %}, lights_info{% endif %});
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
    edge_data[accum_word_index + 0u] = bitcast<u32>(color_sum.x);
    edge_data[accum_word_index + 1u] = bitcast<u32>(color_sum.y);
    edge_data[accum_word_index + 2u] = bitcast<u32>(color_sum.z);
    // Pack (alpha_sum, sample_count_as_float) into the w component —
    // final_blend needs both. We pack them into a vec2<f16>-ish encoding
    // since two values must share one slot; alpha_sum maps to the low
    // bits of bitcast<u32>(f32) is non-trivial, so use the .w slot for
    // sample_count (final blend recomputes alpha as alpha_sum / count
    // via a separate buffer if needed). Stage 3.7 may add a parallel
    // alpha buffer if alpha-resolve quality demands it.
    edge_data[accum_word_index + 3u] = bitcast<u32>(f32(sample_count));
}
{% endif %}
