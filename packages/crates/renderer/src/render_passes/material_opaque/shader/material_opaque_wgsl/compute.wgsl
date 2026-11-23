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
    // classify-dispatch-row: the dispatch is `tile_count` workgroups laid out
    // in rows of CLASSIFY_DISPATCH_ROW (y is 0 until a bucket holds more than
    // one row); the last row's unused tail returns here.
    let tile_slot = wg_id.y * CLASSIFY_DISPATCH_ROW + wg_id.x;
    if (tile_slot >= min(classify_buckets.args[{{ bucket_index }}u].w, classify_buckets.bucket_capacity)) {
        return;
    }
    let tile = classify_buckets.tiles[bucket_offset + tile_slot];
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
    // (Schlick fresnel × ssr_mask; 0 = this surface opts out of SSR), A = ssr_spread
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
                    coords,
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
        // M2a/M2b: PBR opts into SSR by writing its fresnel-weighted
        // reflectivity (Schlick at the geometry n·v — the same normal/view
        // the trace reconstructs — times ssr_mask; see ssr_pbr_descriptor).
        let ssr_desc = ssr_pbr_descriptor(
            material_color.base.rgb,
            material_color.metallic_roughness.x,
            material_color.metallic_roughness.y,
            pbr_material.ssr_mask,
            saturate(dot(world_normal, standard_coordinates.surface_to_camera)),
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
            (material_mesh_meta.receive_shadows & material_mesh_meta.shadow_receiver_gate),
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
    // M2a: material-owned SSR reflection descriptor, once per pixel at
    // sample 0 — reflectivity scaled by the material's MSAA sample coverage
    // (see ssr_descriptor_coverage).
    let ssr_cov = ssr_descriptor_coverage(coords, material_meta_offset);
    textureStore(reflection_descriptor_tex, coords, vec4<f32>(ssr_reflectivity * ssr_cov, ssr_spread));
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
// Fraction of this pixel's MSAA samples covered by the SAME material as the
// sample whose evaluated descriptor is stored (sample 0 by convention). The
// COLOR target gets a proper per-sample edge resolve, but the descriptor is
// single-sample: storing it un-scaled makes the SSR composite add
// all-or-nothing reflection along silhouettes over reflective surfaces —
// alternating bright/dark serration that visibly undoes MSAA (wgsl_validation
// pins this helper). Scaling reflectivity by the writing material's actual
// sample coverage makes the added reflection energy track the same coverage
// the resolved color already encodes: SSR can never add more reflection than
// the reflective surface's share of the pixel.
fn ssr_descriptor_coverage(coords: vec2<i32>, mat_offset: u32) -> f32 {
    {% if multisampled_geometry %}
    var covered = 0u;
    for (var s = 0u; s < {{ msaa_sample_count }}u; s++) {
        var vis_s: vec4<u32>;
        switch(s) {
            case 0u: { vis_s = textureLoad(visibility_data_tex, coords, 0); }
            case 1u: { vis_s = textureLoad(visibility_data_tex, coords, 1); }
            case 2u: { vis_s = textureLoad(visibility_data_tex, coords, 2); }
            case 3u, default: { vis_s = textureLoad(visibility_data_tex, coords, 3); }
        }
        if (join32(vis_s.z, vis_s.w) == mat_offset) {
            covered += 1u;
        }
    }
    return f32(covered) / f32({{ msaa_sample_count }}u);
    {% else %}
    return 1.0;
    {% endif %}
}

// M2a/M2b: the PBR SSR reflection descriptor — RGB = the FULL per-pixel
// reflectivity the SSR pass multiplies its reflection by: Schlick fresnel
// (F0 = mix(0.04, base, metallic) at the shading n·v) times the material's
// ssr_mask. Fresnel is baked HERE, not in the trace: a mask applied to F0
// alone is invisible at grazing (Schlick's (1-F0) term dominates and is
// unmasked), so fractional ssr_mask must scale the finished fresnel to damp
// the reflection uniformly at every angle. 0 = fully opted out (trace + bvh
// skip the pixel; brdf_pbr keeps IBL specular because its suppression reads
// the same masked value). A = GGX roughness mapped to reflection spread
// (0 mirror … 1 diffuse). Single source of truth for the three shading arms
// (cs_opaque / shade_sample / cs_shade interior).
fn ssr_pbr_descriptor(
    base_rgb: vec3<f32>,
    metallic: f32,
    roughness: f32,
    ssr_mask: f32,
    n_dot_v: f32,
) -> vec4<f32> {
    let f0 = mix(vec3<f32>(0.04), base_rgb, metallic);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - saturate(n_dot_v), 5.0);
    return vec4<f32>(
        fresnel * clamp(ssr_mask, 0.0, 1.0),
        roughness,
    );
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

// Per-sample shade output: the shaded color+alpha, plus this sample's SSR
// reflection descriptor (reflectivity rgb + spread; zero when the
// write_ssr_descriptor axis is off, on bails, and for non-reflective
// materials). The caller accumulates the descriptor per-sample so
// final_blend can resolve a COVERAGE-correct per-pixel descriptor — a
// single-sample descriptor makes the SSR composite add all-or-nothing
// reflection along silhouettes, visibly undoing MSAA.
struct ShadeSampleOut {
    color: vec4<f32>,
    ssr_desc: vec4<f32>,
};

// Shade a single MSAA sample for this shader_id and return
// (color+alpha, ssr descriptor). Reads visibility/barycentric/normal
// textures at the given (coords, sample_index). Returns a sentinel zero on
// samples that don't belong to this shader_id (the caller's mask gate
// filters these out upstream, but we keep the bail for robustness).
fn shade_sample(
    coords: vec2<i32>,
    sample_index: u32,
    camera: Camera,
    screen_dims: vec2<u32>,
    screen_dims_f32: vec2<f32>,
    {% if inc.light_access %}
    lights_info: LightsInfo,
    {% endif %}
) -> ShadeSampleOut {
    let textures = msaa_load_sample_textures(coords, sample_index);
    let tri_id = join32(textures.vis_data.x, textures.vis_data.y);
    let mat_meta_off = join32(textures.vis_data.z, textures.vis_data.w);

    // Skybox / no geometry — caller's mask should never put us here
    // for a non-skybox shader_id, but bail safely.
    if (tri_id == U32_MAX) {
        return ShadeSampleOut(vec4<f32>(0.0), vec4<f32>(0.0));
    }
    let sample_mesh_meta = material_mesh_metas[mat_meta_off / META_SIZE_IN_BYTES];
    if (sample_mesh_meta.is_hud == 1u) {
        return ShadeSampleOut(vec4<f32>(0.0), vec4<f32>(0.0));
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
    if (sample_shader_id != {{ shader_id.as_u32() }}u) { return ShadeSampleOut(vec4<f32>(0.0), vec4<f32>(0.0)); }

    var color: vec3<f32>;
    var base_alpha: f32;

    {% if write_ssr_descriptor %}
    // M2a: material-owned SSR reflection descriptor. RGB = reflectivity color
    // (Schlick fresnel × ssr_mask; 0 = this surface opts out of SSR), A = ssr_spread
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
                    coords,
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
        // M2a/M2b: PBR opts into SSR by writing its fresnel-weighted
        // reflectivity (Schlick at this SAMPLE's geometry n·v times ssr_mask;
        // see ssr_pbr_descriptor).
        let ssr_desc = ssr_pbr_descriptor(
            material_color.base.rgb,
            material_color.metallic_roughness.x,
            material_color.metallic_roughness.y,
            pbr_material.ssr_mask,
            saturate(dot(sample_normal, standard_coordinates.surface_to_camera)),
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
            (sample_mesh_meta.receive_shadows & sample_mesh_meta.shadow_receiver_gate),
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
    // M2a: this sample's SSR descriptor rides back to cs_shade_edge, which
    // accumulates it into the slot's descriptor words; final_blend resolves
    // the pixel's coverage-correct descriptor from all samples (edge pixels'
    // primary-pass sample-0 store gets overwritten there).
    return ShadeSampleOut(vec4<f32>(color, base_alpha), vec4<f32>(ssr_reflectivity, ssr_spread));
    {% else %}
    return ShadeSampleOut(vec4<f32>(color, base_alpha), vec4<f32>(0.0));
    {% endif %}
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
    // classify-dispatch-row: the dispatch is `tile_count` workgroups laid out
    // in rows of CLASSIFY_DISPATCH_ROW (y is 0 until a bucket holds more than
    // one row); the last row's unused tail returns here.
    let tile_slot = wg_id.y * CLASSIFY_DISPATCH_ROW + wg_id.x;
    if (tile_slot >= min(classify_buckets.args[{{ bucket_index }}u].w, classify_buckets.bucket_capacity)) {
        return;
    }
    let tile = classify_buckets.tiles[bucket_offset + tile_slot];
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

        // Sample-0 skybox, whether the whole pixel or a silhouette — skybox
        // owns the base color; this pure material kernel never writes it. No
        // need to look at the other samples: the pixel returns either way.
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
                        coords,
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
                pbr_material.ssr_mask,
                saturate(dot(world_normal, standard_coordinates.surface_to_camera)),
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
                (material_mesh_meta.receive_shadows & material_mesh_meta.shadow_receiver_gate),
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
        // MSAA sample 0). Mirrors the cs_opaque store, coverage-scaled.
        let ssr_cov = ssr_descriptor_coverage(coords, material_meta_offset);
        textureStore(reflection_descriptor_tex, coords, vec4<f32>(ssr_reflectivity * ssr_cov, ssr_spread));
        {% endif %}
        return;
    }

    // An edge pixel: `cs_shade_edge` below shades it, in a dispatch of its own.
}

// ════════════════════════════════════════════════════════════════════
// cs_shade_edge — the edge pixels, DENSE
//
// Dispatched over the edge-pixel list (the final-blend args: one workgroup
// per 64 edge pixels), once per bucket, after that bucket's cs_shade.
//
// It is an entry point of its own because a GPU runs 16 to 64 invocations in
// lockstep. Inside cs_shade, one edge pixel made every lane of its group sit
// through per-sample shading, and edges are lines: few pixels, many tiles.
// (M1 Max, 42k edge pixels: 0.6 ms as a band, 8.7 ms one to a tile.) Here
// the cost follows the number of shades, wherever the edges are:
//
//   1. each lane finds the distinct surfaces of its pixel that this bucket
//      owns, and queues one item for each;
//   2. the lanes share the queue out, so the shading has no idle lanes;
//   3. each lane sums its own pixel from the results.
//
// A sample whose visibility, barycentric and normal texels match one already
// queued reuses its result: raster writes per-pixel attributes, so those are
// the same fragment, and shading it again gave the same colour. (Shadow
// visibility is the one per-sample input that can differ; a reused sample
// takes the shadow of the sample it matches, as every interior pixel does.
// The match must stay the same test as cs_prep_edge's.)
// ════════════════════════════════════════════════════════════════════
const EDGE_GROUP_SIZE: u32 = 64u;
const EDGE_NO_ITEM: u32 = 0xFFFFFFFFu;

// Zeroed by WebGPU at the start of every workgroup.
var<workgroup> edge_queue: array<u32, 256>; // lane << 2 | sample
var<workgroup> edge_queued: atomic<u32>;
var<workgroup> edge_lane_xy: array<u32, 64>;
var<workgroup> edge_results: array<ShadeSampleOut, 256>;

@compute @workgroup_size(64)
fn cs_shade_edge(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let edge_pixel_id = gid.x;
    let group_first_id = edge_pixel_id - lane;
    // No returns above the barriers: they need uniform control flow.
    let live = edge_pixel_id < min(edge_data[edge_layout.edge_count_index], edge_layout.max_edge_budget);

    // Our slot in the slot_map.
    var slot_index: u32 = 4u;
    if (live) {
        {% if edge_slot_bits == 16 %}
        let slot_w0 = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id * 2u];
        let slot_w1 = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id * 2u + 1u];
        {% else %}
        let slot_map = edge_data[edge_layout.edge_slot_map_base + edge_pixel_id];
        {% endif %}
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
    }

    // ── 1. Queue this pixel's distinct surfaces ──────────────────────
    var item_of_sample: array<u32, 4>;
    for (var s = 0u; s < 4u; s++) {
        item_of_sample[s] = EDGE_NO_ITEM;
    }
    if (slot_index < 4u) {
        let packed_xy = edge_data[edge_layout.edge_to_xy_base + edge_pixel_id];
        edge_lane_xy[lane] = packed_xy;
        let coords = vec2<i32>(i32(packed_xy & 0xFFFFu), i32(packed_xy >> 16u));

        var uniq_vis: array<vec4<u32>, 4>;
        var uniq_bary: array<vec4<u32>, 4>;
        var uniq_normal: array<vec4<f32>, 4>;
        var uniq_item: array<u32, 4>;
        var uniq_count: u32 = 0u;

        for (var s = 0u; s < 4u; s++) {
            var vis_s: vec4<u32>;
            var bary_s: vec4<u32>;
            var normal_s: vec4<f32>;
            switch(s) {
                case 0u: {
                    vis_s = textureLoad(visibility_data_tex, coords, 0);
                    bary_s = textureLoad(barycentric_tex, coords, 0);
                    normal_s = textureLoad(normal_tangent_tex, coords, 0);
                }
                case 1u: {
                    vis_s = textureLoad(visibility_data_tex, coords, 1);
                    bary_s = textureLoad(barycentric_tex, coords, 1);
                    normal_s = textureLoad(normal_tangent_tex, coords, 1);
                }
                case 2u: {
                    vis_s = textureLoad(visibility_data_tex, coords, 2);
                    bary_s = textureLoad(barycentric_tex, coords, 2);
                    normal_s = textureLoad(normal_tangent_tex, coords, 2);
                }
                case 3u, default: {
                    vis_s = textureLoad(visibility_data_tex, coords, 3);
                    bary_s = textureLoad(barycentric_tex, coords, 3);
                    normal_s = textureLoad(normal_tangent_tex, coords, 3);
                }
            }
            // The gate shade_sample applies: covered, not HUD, this shader id.
            if (join32(vis_s.x, vis_s.y) == U32_MAX) {
                continue;
            }
            let mesh_meta_s = material_mesh_metas[join32(vis_s.z, vis_s.w) / META_SIZE_IN_BYTES];
            if (mesh_meta_s.is_hud == 1u) {
                continue;
            }
            if (material_load_shader_id(mesh_meta_s.material_offset) != {{ shader_id.as_u32() }}u) {
                continue;
            }

            var item = EDGE_NO_ITEM;
            for (var u = 0u; u < uniq_count; u++) {
                if (all(uniq_vis[u] == vis_s) && all(uniq_bary[u] == bary_s) && all(uniq_normal[u] == normal_s)) {
                    item = uniq_item[u];
                    break;
                }
            }
            if (item == EDGE_NO_ITEM) {
                item = atomicAdd(&edge_queued, 1u);
                edge_queue[item] = (lane << 2u) | s;
                uniq_vis[uniq_count] = vis_s;
                uniq_bary[uniq_count] = bary_s;
                uniq_normal[uniq_count] = normal_s;
                uniq_item[uniq_count] = item;
                uniq_count += 1u;
            }
            item_of_sample[s] = item;
        }
    }

    workgroupBarrier();

    // ── 2. Shade the queue, every lane working ───────────────────────
    let queued = atomicLoad(&edge_queued);
    if (lane < queued) {
        let edge_camera = camera_from_raw(camera_raw);
        let edge_screen_dims_u = textureDimensions(visibility_data_tex);
        let edge_screen_dims = vec2<u32>(edge_screen_dims_u.x, edge_screen_dims_u.y);
        let edge_screen_dims_f32 = vec2<f32>(f32(edge_screen_dims.x), f32(edge_screen_dims.y));
        {% if inc.light_access %}
        let lights_info = get_lights_info();
        {% endif %}
        for (var item = lane; item < queued; item += EDGE_GROUP_SIZE) {
            let queued_item = edge_queue[item];
            let owner = queued_item >> 2u;
            let s = queued_item & 3u;
            let packed_xy = edge_lane_xy[owner];
            let coords = vec2<i32>(i32(packed_xy & 0xFFFFu), i32(packed_xy >> 16u));
            {% if prep_present %}
            // EDGE mode: the compact per-edge-sample shadow buffer.
            g_prep_ctx = PrepReadContext(PREP_MODE_EDGE, coords, prep_edge_shadow_xy(group_first_id + owner, s));
            {% endif %}
            edge_results[item] = shade_sample(coords, s, edge_camera, edge_screen_dims, edge_screen_dims_f32{% if inc.light_access %}, lights_info{% endif %});
        }
    }

    workgroupBarrier();

    // ── 3. Sum this pixel and accumulate ─────────────────────────────
    var color_sum = vec3<f32>(0.0);
    var sample_count: u32 = 0u;
    {% if write_ssr_descriptor %}
    var desc_rgb_sum = vec3<f32>(0.0);
    var desc_spread_wsum: f32 = 0.0;
    {% endif %}
    for (var s = 0u; s < 4u; s++) {
        let item = item_of_sample[s];
        if (item == EDGE_NO_ITEM) {
            continue;
        }
        let ss = edge_results[item];
        // Tonemapped-space resolve; rationale in final_blend.wgsl. Store
        // Σ t(s) per channel with t(s) = s/(1+s); final_blend averages
        // then inverts. The weight word carries the plain SAMPLE COUNT.
        color_sum += ss.color.rgb / (vec3<f32>(1.0) + ss.color.rgb);
        sample_count += 1u;
        {% if write_ssr_descriptor %}
        // SSR descriptor accumulation — RAW sums (final_blend divides by
        // the 4 MSAA samples); spread weighted by its own reflectivity.
        desc_rgb_sum += ss.ssr_desc.rgb;
        desc_spread_wsum += ss.ssr_desc.a
            * max(ss.ssr_desc.r, max(ss.ssr_desc.g, ss.ssr_desc.b));
        {% endif %}
    }
    if (sample_count == 0u) {
        return;
    }

    // Accumulate into accumulator[edge_pixel_id × 4 + slot_index], the format
    // final_blend resolves. SSR on: 8 words per slot (0..4 Karis color sum +
    // weight, 4..8 SSR descriptor sums). SSR off: 4 words — the descriptor
    // half is not ALLOCATED (see ACCUMULATOR_SLOT_BYTES in edge_buffers.rs:
    // the wide stride cost ~32 MB of idle VRAM at the desktop edge budget).
    let accum_word_index = edge_layout.accumulator_base + (edge_pixel_id * 4u + slot_index) * {% if write_ssr_descriptor %}8u{% else %}4u{% endif %};
    edge_data[accum_word_index + 0u] = bitcast<u32>(color_sum.x);
    edge_data[accum_word_index + 1u] = bitcast<u32>(color_sum.y);
    edge_data[accum_word_index + 2u] = bitcast<u32>(color_sum.z);
    // Plain SAMPLE COUNT (tonemapped-space resolve — see final_blend.wgsl).
    edge_data[accum_word_index + 3u] = bitcast<u32>(f32(sample_count));
    {% if write_ssr_descriptor %}
    edge_data[accum_word_index + 4u] = bitcast<u32>(desc_rgb_sum.x);
    edge_data[accum_word_index + 5u] = bitcast<u32>(desc_rgb_sum.y);
    edge_data[accum_word_index + 6u] = bitcast<u32>(desc_rgb_sum.z);
    edge_data[accum_word_index + 7u] = bitcast<u32>(desc_spread_wsum);
    {% endif %}
}
{% endif %}
