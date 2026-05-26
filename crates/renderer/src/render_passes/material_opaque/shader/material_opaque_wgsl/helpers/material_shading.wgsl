// Helper functions for material shading.
//
// Pre-Priority-3 this file hosted msaa_resolve_samples + msaa_process_sample
// (the 4x-unrolled MSAA edge-resolve that produced the SPIR-V bloat which
// Android's Vulkan driver rejected). Both were deleted; edge resolution
// now happens in per-shader-id `material_edge_resolve_{shader_id}` compute
// pipelines + a final_blend compositor, all driven off classify's per-edge
// sample lists. See docs/plans/more-optimizations.md § Priority 3.
//
// The legacy `MsaaSampleResult` / `MsaaSampleTextures` types + the helpers
// that *don't* duplicate the full shading kernel (`msaa_load_sample_textures`,
// `msaa_apply_instance_tint`) remain — the new edge_resolve shaders reuse
// them as the canonical MSAA sample-loader / instance-tint passthrough.

// Result from shading a single MSAA sample.
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

// ----------------------------------------------------------
// LEGACY (pre-Priority-3) FUNCTIONS BELOW — DELETED.
// ----------------------------------------------------------
// `msaa_process_sample` (the ~150-line shading kernel that branched per
// shader_id and inlined the full PBR path) and `msaa_resolve_samples`
// (the 4x-unrolled caller) used to live here. The 4x inline copies of
// the entire shading pipeline are what blew Android Vulkan's SPIR-V
// complexity ceiling. Edge shading is now per-shader-id +
// indirect-dispatched off the classify pass's sample lists, so each
// edge_resolve_{shader_id} pipeline only contains its own shading code
// once.
//
// The original signature for reference (deleted body):
//
// fn msaa_process_sample(camera, coords, screen_dims_f32, lights_info,
//                        standard_coordinates, textures) -> MsaaSampleResult;
//
// fn msaa_resolve_samples(camera, coords, screen_dims, screen_dims_f32,
//                         lights_info) -> MsaaResolveResult;

// (Placeholder so the closing askama-endif below still has a body.)
fn _msaa_helpers_present() -> u32 { return 0u; }
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
