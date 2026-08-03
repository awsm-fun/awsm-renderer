// Helper functions for material shading.
//
// The pre-Stage-3 `msaa_resolve_samples` / `msaa_process_sample` /
// `msaa_apply_instance_tint` cross-shader resolve helpers have been
// removed: MSAA edge resolution is now owned end-to-end by the
// classify → per-shader edge_resolve → final_blend pipeline chain
// (see https://github.com/dakom/awsm-renderer/pull/99 § Priority 3). The legacy
// helpers carried a runtime switch over every registered shader_id,
// which inlined every material's shading body into every primary
// pipeline's SPIR-V — an O(N) bloat in the number of dynamic
// materials. Stage 3's per-shader-id specialization eliminates that.
//
// What remains here (ALL gated on `multisampled_geometry` — absent without MSAA):
//   * `MsaaSampleTextures` — struct used by the `cs_edge` entry point
//   * `msaa_load_sample_textures` — used by the `cs_edge` entry point
//
// Primary opaque (compute.wgsl) shades only sample-0 directly; the
// final_blend dispatch overwrites edge pixels with the proper
// 4-sample average from Stage 3's per-shader-id pipelines.

{% if multisampled_geometry %}
// Texture data loaded for a single MSAA sample. (Gated with its only consumer,
// `msaa_load_sample_textures`, so the single-sampled module carries no MSAA code.)
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

// Load texture data for a single MSAA sample. Called by per-shader
// `edge_resolve.wgsl` to pull a sample's visibility/barycentric/normal
// data so it can be shaded with this pipeline's specialized
// shader_id. Not used by primary opaque (which shades sample-0
// directly).
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

{% endif %}

{# Skinny materials: compute_material_color is the PBR builder's entry point
   (calls pbr_get_material_color* + pbr_get_gradients). Only the base==Pbr
   dispatch calls it, so gate it with the PBR material-color include. #}
{% if inc.material_color_calc %}
{% match mipmap %}
    {% when MipmapMode::Gradient %}
        // ── Geometric specular AA (Kaplanyan '13 / Tokuyoshi '19, G-buffer
        // form) ─────────────────────────────────────────────────────────────
        // Widen GGX roughness by the pixel's screen-space GEOMETRY-normal
        // variance. Sub-pixel normal detail — plate bevels between coplanar
        // floor tiles, tight rim curvature — collapses to one shade per
        // pixel and fires HDR glints under punctual lights that crawl with
        // the camera ("shimmering dots"). The edge classifier can't help:
        // shallow bevels sit under its 18° normal threshold, same mesh, no
        // depth step. Estimating dN from the geometry normal G-buffer's
        // 4-neighborhood (cache-hot: edge classification just read them)
        // and folding it into alpha widens exactly those highlights into
        // stable bands; smooth interiors measure ~zero variance and are
        // untouched. Uses the min of forward/backward differences per axis
        // so an unrelated surface across a silhouette can't inflate the
        // estimate — a real crease shows on both sides.
        const SPEC_AA_NORMAL_STRENGTH: f32 = 0.25;
        const SPEC_AA_NORMAL_MAX_KERNEL: f32 = 0.18;

        fn spec_aa_widen_roughness(
            perceptual_roughness: f32,
            coords: vec2<i32>,
            center_n: vec3<f32>,
        ) -> f32 {
            let n_r = unpack_normal_tangent(textureLoad(normal_tangent_tex, coords + vec2<i32>(1, 0), 0)).N;
            let n_l = unpack_normal_tangent(textureLoad(normal_tangent_tex, coords - vec2<i32>(1, 0), 0)).N;
            let n_d = unpack_normal_tangent(textureLoad(normal_tangent_tex, coords + vec2<i32>(0, 1), 0)).N;
            let n_u = unpack_normal_tangent(textureLoad(normal_tangent_tex, coords - vec2<i32>(0, 1), 0)).N;
            let dr = n_r - center_n;
            let dl = n_l - center_n;
            let dd = n_d - center_n;
            let du = n_u - center_n;
            let dx2 = min(dot(dr, dr), dot(dl, dl));
            let dy2 = min(dot(dd, dd), dot(du, du));
            let kernel2 = min(
                2.0 * SPEC_AA_NORMAL_STRENGTH * (dx2 + dy2),
                SPEC_AA_NORMAL_MAX_KERNEL,
            );
            let alpha = perceptual_roughness * perceptual_roughness;
            let alpha2 = min(alpha * alpha + kernel2, 1.0);
            // alpha² → alpha → perceptual.
            return sqrt(sqrt(alpha2));
        }

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
            color_sets_index: u32,
            geometry_tbn: TBN,
            bary_derivs: vec4<f32>,
            screen_coords: vec2<i32>,
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

            var color = pbr_get_material_color_grad(
                triangle_indices,
                attribute_data_offset,
                triangle_index,
                pbr_material,
                barycentric,
                vertex_attribute_stride,
                uv_sets_index,
                color_sets_index,
                gradients,
                geometry_tbn,
            );
            color.metallic_roughness.y = spec_aa_widen_roughness(
                color.metallic_roughness.y,
                screen_coords,
                geometry_tbn.N,
            );
            return color;
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
            color_sets_index: u32,
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
                color_sets_index,
                geometry_tbn,
            );
        }
{% endmatch %}
{% endif %}{# end inc.material_color_calc (compute_material_color) #}
