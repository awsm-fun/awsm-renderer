// compute.wgsl — the opaque MATERIAL kernel (skybox-free; the canonical skybox
// bucket uses skybox_primary.wgsl instead). Shared preamble is factored out.
{% include "material_opaque_wgsl/opaque_kernel_includes.wgsl" %}

{% if prep_present %}
// Plan B (stage 5a/5b): a per-thread context each entry point sets once, that the
// shared texture_uv() / vertex_color() / shadow helpers branch on — so ONE set
// of helpers serves cs_opaque (PRIMARY, reads the prep array at sample-0 coords)
// AND cs_edge (5b-shadow: EDGE reads the compact per-edge-sample shadow buffer;
// UV/vcolor still RECOMPUTE — 5b-attrs deferred) AND the non-prep path, with no
// forked MSAA copies.
//   PRIMARY   — cs_opaque: shadow source = full-screen prep_shadow_visibility at
//               `coords` (sample 0).
//   EDGE      — cs_edge per sample: shadow source = compact prep_edge_shadow at
//               `edge_shadow_xy` (= the 2D texel for this edge_pixel × sample);
//               UV/vcolor recompute (5b-attrs deferred).
//   RECOMPUTE — non-prep / fallback: inline-sample shadows + recompute attrs.
const PREP_MODE_RECOMPUTE: u32 = 0u;
const PREP_MODE_PRIMARY: u32 = 1u;
const PREP_MODE_EDGE: u32 = 2u;
struct PrepReadContext { mode: u32, coords: vec2<i32>, edge_shadow_xy: vec2<i32> }
var<private> g_prep_ctx: PrepReadContext;
{% if multisampled_geometry %}
// Stage 5b-shadow: the compact per-edge-sample shadow buffer key. MUST match
// material_prep's cs_prep_edge: `idx = edge_pixel_id * MAX_EDGE_SHADOW_SAMPLES +
// sample`, 2D coords `(idx % W, idx / W)`; layer = slot/4 (read in apply_lighting).
const PREP_EDGE_SHADOW_SAMPLES: u32 = 4u;
const PREP_EDGE_SHADOW_TEX_WIDTH: u32 = {{ edge_shadow_tex_width }}u;
fn prep_edge_shadow_xy(edge_pixel_id: u32, sample: u32) -> vec2<i32> {
    let idx = edge_pixel_id * PREP_EDGE_SHADOW_SAMPLES + sample;
    return vec2<i32>(i32(idx % PREP_EDGE_SHADOW_TEX_WIDTH), i32(idx / PREP_EDGE_SHADOW_TEX_WIDTH));
}
{% endif %}
{% endif %}


{% if !multisampled_geometry %}
// Non-MSAA interior shading. Under MSAA the renderer dispatches `cs_shade`
// (interior + edge in one kernel) instead, so `cs_opaque` is NOT emitted in
// the multisampled module — invariant: a compiled module carries only the
// entry points its AA config dispatches (no cross-AA code).
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
    // for one shader_id. Data-driven layout: read this pipeline's own tile
    // offset by its bucket index (resolved at template-render time, same value
    // the old per-name `<name>_offset` field held) from the `offsets` array —
    // no O(N) per-bucket field walk.
    let bucket_offset = classify_buckets.offsets[{{ bucket_index }}u];
    let tile = classify_buckets.tiles[bucket_offset + wg_id.x];
    let coords = vec2<i32>(i32(tile.x * 8u + lid.x), i32(tile.y * 8u + lid.y));
    {% if prep_present %}g_prep_ctx = PrepReadContext(PREP_MODE_PRIMARY, coords, vec2<i32>(0, 0));{% endif %}
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

    {% if write_ssr_descriptor %}
    // M2a: material-owned SSR reflection descriptor. RGB = reflectivity color
    // (ssr_mask * ssr_tint; 0 = this surface opts out of SSR), A = ssr_spread
    // (0 mirror … 1 diffuse). Defaults to "no reflection"; the PBR arm below
    // opts in. Stored once per pixel at sample 0 beside the HDR write. Compiled
    // out entirely when SSR is off (write_ssr_descriptor = false).
    var ssr_reflectivity: vec3<f32> = vec3<f32>(0.0);
    var ssr_spread: f32 = 0.0;
    {% endif %}

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
        {% if write_ssr_descriptor %}
        // M2a/M2b: PBR opts into SSR by writing its specular reflectance F0 —
        // dielectrics ~0.04 grey (weak at normal, →white at grazing via Schlick
        // in the SSR pass), metals = base color (strong, tinted). The GGX
        // roughness maps to reflection spread (0 mirror … 1 diffuse blur).
        let ssr_desc = ssr_pbr_descriptor(
            material_color.base.rgb,
            material_color.metallic_roughness.x,
            material_color.metallic_roughness.y,
        );
        ssr_reflectivity = ssr_desc.rgb;
        ssr_spread = ssr_desc.a;
        {% endif %}
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
            tbn.T,
            tbn.B,
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
    {% if write_ssr_descriptor %}
    // M2a: material-owned SSR reflection descriptor, once per pixel at sample 0.
    textureStore(reflection_descriptor_tex, coords, vec4<f32>(ssr_reflectivity, ssr_spread));
    {% endif %}
}
{% endif %}

fn get_triangle_indices(attribute_indices_offset: u32, triangle_index: u32) -> vec3<u32> {
    let base = attribute_indices_offset + (triangle_index * 3u);
    return vec3<u32>(
        bitcast<u32>(visibility_data[base]),
        bitcast<u32>(visibility_data[base + 1u]),
        bitcast<u32>(visibility_data[base + 2u]),
    );
}

{% if write_ssr_descriptor %}
// M2a/M2b: the PBR SSR reflection descriptor — RGB = specular reflectance F0
// (dielectrics ~0.04 grey, ramping to white at grazing via Schlick in the SSR
// pass; metals = base color, strong + tinted), A = GGX roughness mapped to
// reflection spread (0 mirror … 1 diffuse). Single source of truth for the
// three shading arms (cs_opaque / shade_sample / cs_shade interior).
fn ssr_pbr_descriptor(base_rgb: vec3<f32>, metallic: f32, roughness: f32) -> vec4<f32> {
    return vec4<f32>(mix(vec3<f32>(0.04), base_rgb, metallic), roughness);
}
{% endif %}

{% if multisampled_geometry %}
// ════════════════════════════════════════════════════════════════════
// UNIFIED MODULE — shared MSAA per-sample shading helper.
//
// `shade_sample` is the per-sample shading body used by the `cs_shade`
// entry point below (it shades each MSAA edge sample this shader_id owns
// + accumulates into the per-material accumulator slot). It shares the
// embedded per-material shading + the dynamic-material wrapper + all
// helper includes already pulled in by the shared preamble above (each
// global/binding/fn appears exactly once across both entry points). The
// `edge_data` / `edge_layout` bindings cs_shade reads are declared (gated
// on `multisampled_geometry`) in bind_groups.wgsl at group(3) bindings
// 10/11.
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

    {% if write_ssr_descriptor %}
    // M2a: material-owned SSR reflection descriptor. RGB = reflectivity color
    // (ssr_mask * ssr_tint; 0 = this surface opts out of SSR), A = ssr_spread
    // (0 mirror … 1 diffuse). Defaults to "no reflection"; the PBR arm below
    // opts in. Stored once per pixel at sample 0 beside the HDR write. Compiled
    // out entirely when SSR is off (write_ssr_descriptor = false).
    var ssr_reflectivity: vec3<f32> = vec3<f32>(0.0);
    var ssr_spread: f32 = 0.0;
    {% endif %}

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
        {% if write_ssr_descriptor %}
        // M2a/M2b: PBR opts into SSR by writing its specular reflectance F0 —
        // dielectrics ~0.04 grey (weak at normal, →white at grazing via Schlick
        // in the SSR pass), metals = base color (strong, tinted). The GGX
        // roughness maps to reflection spread (0 mirror … 1 diffuse blur).
        let ssr_desc = ssr_pbr_descriptor(
            material_color.base.rgb,
            material_color.metallic_roughness.x,
            material_color.metallic_roughness.y,
        );
        ssr_reflectivity = ssr_desc.rgb;
        ssr_spread = ssr_desc.a;
        {% endif %}
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
            sample_tbn.T,
            sample_tbn.B,
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

    {% if write_ssr_descriptor %}
    // M2a: MSAA EDGE pixels — the bucket that owns sample 0 stores the pixel's
    // SSR reflection descriptor, mirroring the interior arm's / cs_opaque's
    // sample-0 convention (and the SSR trace's sample-0 depth read). Exactly
    // one shader_id owns a given sample, so the write is race-free. The
    // descriptor texture is never cleared, so without this store edge pixels
    // kept stale prior-frame reflectivity. Sky-at-sample-0 edge pixels get no
    // store (the U32_MAX bail above) — the trace bails on depth >= 1.0 there
    // before ever reading the descriptor.
    if (sample_index == 0u) {
        textureStore(reflection_descriptor_tex, coords, vec4<f32>(ssr_reflectivity, ssr_spread));
    }
    {% endif %}

    return vec4<f32>(color, base_alpha);
}

// ════════════════════════════════════════════════════════════════════
// UNIFIED MODULE — `cs_shade` entry point (U1, unified-edge-shading.md).
//
// ONE kernel merging `cs_opaque` (interior, sample 0 → opaque_tex) +
// `cs_edge` (edge, per-sample → accumulator slot) into a single body,
// driven by `edge_id_tex` + the U0 ANY-sample tile dispatch (NOT the
// per-bucket edge-sample lists). Dispatched over THIS bucket's tile list
// (8×8 tile = workgroup, 1 thread/pixel) exactly like `cs_opaque`.
//
// Per pixel: read `edge_id_tex` ONCE.
//   * `edge_id == U32_MAX` (interior): do EXACTLY what `cs_opaque` does —
//     shade sample 0 (with the same per-pixel shader_id guard) and write
//     opaque_tex; else skip. The body below is `cs_opaque`'s body verbatim.
//   * else (edge): do EXACTLY what `cs_edge` does for this bucket's owned
//     samples — call the SAME `shade_sample(coords, s, ...)` and accumulate
//     into this material's accumulator slot via the SAME `edge_slot_map`
//     slot-find + the SAME accumulate write. The ONLY difference from
//     `cs_edge` is where (edge_id, which-samples) comes from: `edge_id_tex`
//     + reading the 4 per-sample materials at this pixel, instead of the
//     compact edge-sample-list entry.
//
// Reuses the existing per-material accumulator + edge_slot_map + final_blend
// resolve UNCHANGED (this is what makes the toggle-ON output byte-identical
// to the toggle-OFF cs_opaque+cs_edge path). The OLD cs_opaque/cs_edge entry
// points stay in this module for the toggle-OFF path.
// ════════════════════════════════════════════════════════════════════
@compute @workgroup_size(8, 8)
fn cs_shade(
    @builtin(workgroup_id) wg_id: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>
) {
    // Tile lookup — identical to cs_opaque (this bucket's ANY-sample tile
    // list; `workgroup_id.x` is the bucket entry index, `local_invocation_id`
    // the 8×8 thread → pixel offset).
    let bucket_offset = classify_buckets.offsets[{{ bucket_index }}u];
    let tile = classify_buckets.tiles[bucket_offset + wg_id.x];
    let coords = vec2<i32>(i32(tile.x * 8u + lid.x), i32(tile.y * 8u + lid.y));
    let screen_dims = textureDimensions(opaque_tex);
    let screen_dims_i32 = vec2<i32>(i32(screen_dims.x), i32(screen_dims.y));
    let screen_dims_f32 = vec2<f32>(f32(screen_dims.x), f32(screen_dims.y));

    // Bounds check (same as cs_opaque).
    if (coords.x >= screen_dims_i32.x || coords.y >= screen_dims_i32.y) {
        return;
    }

    // Read the per-pixel edge-id ONCE. U32_MAX → interior; else → edge
    // (the compact edge_pixel_id = the accumulator base).
    let edge_id = textureLoad(edge_id_tex, coords).x;

    if (edge_id == U32_MAX) {
        // ── INTERIOR ARM (cs_opaque body, verbatim) ──────────────────
        {% if prep_present %}g_prep_ctx = PrepReadContext(PREP_MODE_PRIMARY, coords, vec2<i32>(0, 0));{% endif %}
        let pixel_center = vec2<f32>(f32(coords.x) + 0.5, f32(coords.y) + 0.5);

        let visibility_data_info = textureLoad(visibility_data_tex, coords, 0);

        let triangle_index = join32(visibility_data_info.x, visibility_data_info.y);
        let material_meta_offset = join32(visibility_data_info.z, visibility_data_info.w);

        let camera = camera_from_raw(camera_raw);
        let frame_globals = frame_globals_from_raw(frame_globals_raw);

        // early return if we only hit skybox / no geometry (for all samples).
        // This is the pure material kernel — it never writes the skybox.
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
            return;
        }

        // Sample-0 skybox at a silhouette edge — skybox owns the base color.
        if (triangle_index == U32_MAX) {
            return;
        }

        let material_mesh_meta = material_mesh_metas[material_meta_offset / META_SIZE_IN_BYTES];
        if (material_mesh_meta.is_hud == 1u) {
            return;
        }

        let barycentric_raw = textureLoad(barycentric_tex, coords, 0);
        let bary_xy = vec2<f32>(f32(barycentric_raw.x), f32(barycentric_raw.y)) / 65535.0;
        let barycentric = vec3<f32>(bary_xy.x, bary_xy.y, 1.0 - bary_xy.x - bary_xy.y);
        let main_instance_id = join32(barycentric_raw.z, barycentric_raw.w);

        let material_offset = material_mesh_meta.material_offset;
        let shader_id = material_load_shader_id(material_offset);

        // Per-pixel `shader_id` guard (same as cs_opaque).
        if (shader_id != {{ shader_id.as_u32() }}u) { return; }

        let vertex_attribute_stride = material_mesh_meta.vertex_attribute_stride / 4;
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

        let packed_nt = textureLoad(normal_tangent_tex, coords, 0);
        let tbn = unpack_normal_tangent(packed_nt);
        let world_normal = tbn.N;

        {% if inc.light_access %}
        let lights_info = get_lights_info();
        {% endif %}

        var color: vec3<f32>;
        var base_alpha: f32;
        {% if write_ssr_descriptor %}
        // M2a: SSR reflection descriptor (interior arm). See cs_opaque.
        var ssr_reflectivity: vec3<f32> = vec3<f32>(0.0);
        var ssr_spread: f32 = 0.0;
        {% endif %}

        {% if base == ShadingBase::Unlit %}
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
            {% if write_ssr_descriptor %}
            // M2a: PBR opts into SSR (interior arm). See ssr_pbr_descriptor.
            let ssr_desc = ssr_pbr_descriptor(
                material_color.base.rgb,
                material_color.metallic_roughness.x,
                material_color.metallic_roughness.y,
            );
            ssr_reflectivity = ssr_desc.rgb;
            ssr_spread = ssr_desc.a;
            {% endif %}
        {% else if base == ShadingBase::Flipbook %}
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
                tbn.T,
                tbn.B,
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

        {% if debug.normals %}
            textureStore(opaque_tex, coords, vec4<f32>(debug_normals(world_normal), 1.0));
            return;
        {% endif %}

        if (main_instance_id != INSTANCE_ATTR_NONE) {
            let attr = instance_attrs[main_instance_id];
            let tint = unpack4x8unorm(attr.color_packed);
            color = color * tint.rgb;
            base_alpha = base_alpha * tint.a * attr.alpha;
        }

        {% if debug.views %}
        if (cull_params.debug_wireframe == 1u) {
            let wire_edge = min(min(barycentric.x, barycentric.y), barycentric.z);
            let wire = 1.0 - smoothstep(0.0, 0.02, wire_edge);
            color = mix(vec3<f32>(0.55, 0.57, 0.60), vec3<f32>(0.05, 0.05, 0.07), wire);
        }
        {% endif %}

        textureStore(opaque_tex, coords, vec4<f32>(color, base_alpha));
        {% if write_ssr_descriptor %}
        // M2a: material-owned SSR reflection descriptor (cs_shade interior arm,
        // MSAA sample 0). Mirrors the cs_opaque store.
        textureStore(reflection_descriptor_tex, coords, vec4<f32>(ssr_reflectivity, ssr_spread));
        {% endif %}
        return;
    }

    // ── EDGE ARM (cs_edge per-sample shade + accumulate, verbatim) ────
    // `edge_id` (= the compact edge_pixel_id) is this pixel's accumulator
    // base. We reconstruct the per-sample ownership mask cs_edge read from
    // the compact list by reading the 4 sample materials at this pixel and
    // testing each against THIS bucket's shader_id (the same gate
    // `shade_sample` applies); the slot-find against `edge_slot_map` then
    // gates the single accumulator write to exactly this material's slot
    // (identical to cs_edge — a sample owned by a different bucket sharing
    // our shader_id finds no matching slot here and is skipped).
    let edge_pixel_id = edge_id;

    // Find our slot in the slot_map (IDENTICAL to cs_edge).
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

    let edge_camera = camera_from_raw(camera_raw);
    let edge_screen_dims_u = textureDimensions(visibility_data_tex);
    let edge_screen_dims = vec2<u32>(edge_screen_dims_u.x, edge_screen_dims_u.y);
    let edge_screen_dims_f32 = vec2<f32>(f32(edge_screen_dims.x), f32(edge_screen_dims.y));
    {% if inc.light_access %}
    let lights_info = get_lights_info();
    {% endif %}{% if prep_present %}
    // EDGE mode (same as cs_edge): read the compact per-edge-sample shadow
    // buffer; `edge_shadow_xy` is set PER SAMPLE in the loop below.
    g_prep_ctx = PrepReadContext(PREP_MODE_EDGE, coords, vec2<i32>(0, 0));
{% endif %}
    var color_sum = vec3<f32>(0.0);
    var alpha_sum: f32 = 0.0;
    var sample_count: u32 = 0u;
    var weight_sum: f32 = 0.0;

    for (var s = 0u; s < 4u; s++) {
        // Per-sample ownership: the same shader_id gate `shade_sample`
        // applies. Reading the sample material here reconstructs cs_edge's
        // compact-list sample_mask (which-samples-this-bucket-owns).
        var owns_sample = false;
        {
            var vis_s: vec4<u32>;
            switch(s) {
                case 0u: { vis_s = textureLoad(visibility_data_tex, coords, 0); }
                case 1u: { vis_s = textureLoad(visibility_data_tex, coords, 1); }
                case 2u: { vis_s = textureLoad(visibility_data_tex, coords, 2); }
                case 3u, default: { vis_s = textureLoad(visibility_data_tex, coords, 3); }
            }
            let tri_s = join32(vis_s.x, vis_s.y);
            if (tri_s != U32_MAX) {
                let mat_off_s = join32(vis_s.z, vis_s.w);
                let mesh_meta_s = material_mesh_metas[mat_off_s / META_SIZE_IN_BYTES];
                if (mesh_meta_s.is_hud != 1u) {
                    let sid_s = material_load_shader_id(mesh_meta_s.material_offset);
                    if (sid_s == {{ shader_id.as_u32() }}u) {
                        owns_sample = true;
                    }
                }
            }
        }
        if (owns_sample) {
            {% if prep_present %}
            g_prep_ctx.edge_shadow_xy = prep_edge_shadow_xy(edge_pixel_id, s);
            {% endif %}
            let shaded = shade_sample(coords, s, edge_camera, edge_screen_dims, edge_screen_dims_f32{% if inc.light_access %}, lights_info{% endif %});
            // Karis (tonemap-weighted) resolve; rationale in final_blend.wgsl.
            let karis_w = 1.0 / (1.0 + max(shaded.r, max(shaded.g, shaded.b)));
            color_sum += shaded.rgb * karis_w;
            alpha_sum += shaded.a;
            sample_count += 1u;
            weight_sum += karis_w;
        }
    }

    if (sample_count == 0u) {
        return;
    }

    // Accumulate into accumulator[edge_pixel_id × 4 + slot_index] (IDENTICAL
    // to cs_edge — same format the unchanged final_blend resolves).
    let accum_word_index = edge_layout.accumulator_base + (edge_pixel_id * 4u + slot_index) * 4u;
    edge_data[accum_word_index + 0u] = bitcast<u32>(color_sum.x);
    edge_data[accum_word_index + 1u] = bitcast<u32>(color_sum.y);
    edge_data[accum_word_index + 2u] = bitcast<u32>(color_sum.z);
    // Karis WEIGHT sum, not the raw sample count.
    edge_data[accum_word_index + 3u] = bitcast<u32>(weight_sum);
}
{% endif %}
