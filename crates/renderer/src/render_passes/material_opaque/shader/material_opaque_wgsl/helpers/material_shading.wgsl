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
// What remains here:
//   * `MsaaSampleTextures` — struct used by per-shader `edge_resolve.wgsl`
//   * `msaa_load_sample_textures` — used by per-shader `edge_resolve.wgsl`
//
// Primary opaque (compute.wgsl) shades only sample-0 directly; the
// final_blend dispatch overwrites edge pixels with the proper
// 4-sample average from Stage 3's per-shader-id pipelines.

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
{% endif %}{# end inc.material_color_calc (compute_material_color) #}
