//! Rendered-source tests for the geometry shader family (plain, masked,
//! custom-vertex, masked + custom-vertex).

use awsm_renderer_materials::MaterialShaderId;

use crate::dynamic_materials::ShadingBase;
use crate::render_passes::geometry::shader::{
    cache_key::{DynamicVertexShaderInfo, ShaderCacheKeyGeometry},
    custom_vertex_cache_key::ShaderCacheKeyGeometryCustomVertex,
    custom_vertex_template::ShaderTemplateGeometryCustomVertex,
    masked_cache_key::ShaderCacheKeyGeometryMasked,
    masked_custom_vertex_cache_key::ShaderCacheKeyGeometryMaskedCustomVertex,
    masked_custom_vertex_template::ShaderTemplateGeometryMaskedCustomVertex,
    masked_template::ShaderTemplateGeometryMasked,
    template::ShaderTemplateGeometry,
};

fn dynamic_vertex() -> DynamicVertexShaderInfo {
    DynamicVertexShaderInfo {
        shader_includes: Default::default(),
        struct_decl: "struct MaterialData { amp: f32, }".to_string(),
        loader_decl: "fn material_data_load(byte_offset: u32) -> MaterialData { \
                      return MaterialData(bitcast<f32>(materials[byte_offset / 4u])); }"
            .to_string(),
        wgsl_vertex: "var out: VertexDisplaceOutput; \
                      out.position = input.position + input.normal * input.material.amp * sin(input.globals.time); \
                      out.normal = input.normal; out.tangent = input.tangent; return out;"
            .to_string(),
    }
}

/// Every geometry-family variant the renderer can build, rendered, for the
/// given AA targets: `(label, wgsl)`.
pub(crate) fn rendered_variants(msaa: Option<u32>, velocity: bool) -> Vec<(String, String)> {
    // TAA's geometry path: the velocity variant carries the mip bias too.
    rendered_variants_with(msaa, velocity, velocity)
}

/// [`rendered_variants`] with the TAA texture-LOD-bias axis set separately
/// (`mip_bias` without `velocity` = the prep-pass fallback's targets).
pub(crate) fn rendered_variants_with(
    msaa: Option<u32>,
    velocity: bool,
    mip_bias: bool,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (instancing, storage) in [(false, true), (false, false), (true, false)] {
        let key = ShaderCacheKeyGeometry {
            instancing_transforms: instancing,
            meta_storage_array: storage,
            msaa_samples: msaa,
            velocity,
            mip_bias,
            dynamic_vertex_shader: None,
        };
        let src = ShaderTemplateGeometry::try_from(&key)
            .unwrap()
            .into_source()
            .unwrap();
        out.push((format!("geometry inst={instancing} storage={storage}"), src));
    }
    for base in [ShadingBase::Pbr, ShadingBase::Flipbook] {
        let key = ShaderCacheKeyGeometryMasked {
            texture_pool_arrays_len: 2,
            texture_pool_samplers_len: 2,
            msaa_samples: msaa,
            velocity,
            mip_bias,
            shader_id: MaterialShaderId::PBR,
            base,
            dynamic_alpha: None,
        };
        let src = ShaderTemplateGeometryMasked::try_from(&key)
            .unwrap()
            .into_source()
            .unwrap();
        out.push((format!("masked {base:?}"), src));
    }
    for instancing in [false, true] {
        let key = ShaderCacheKeyGeometryCustomVertex {
            shader_id: MaterialShaderId::PBR,
            dynamic_vertex: dynamic_vertex(),
            texture_pool_arrays_len: 2,
            texture_pool_samplers_len: 2,
            msaa_samples: msaa,
            velocity,
            mip_bias,
            instancing_transforms: instancing,
            meta_storage_array: false,
        };
        let src = ShaderTemplateGeometryCustomVertex::try_from(&key)
            .unwrap()
            .into_source()
            .unwrap();
        out.push((format!("custom-vertex inst={instancing}"), src));
    }
    let key = ShaderCacheKeyGeometryMaskedCustomVertex {
        shader_id: MaterialShaderId::PBR,
        base: ShadingBase::Pbr,
        dynamic_vertex: dynamic_vertex(),
        dynamic_alpha: None,
        texture_pool_arrays_len: 2,
        texture_pool_samplers_len: 2,
        msaa_samples: msaa,
        velocity,
        mip_bias,
    };
    let src = ShaderTemplateGeometryMaskedCustomVertex::try_from(&key)
        .unwrap()
        .into_source()
        .unwrap();
    out.push(("masked-custom-vertex".to_string(), src));
    out
}
