//! Shader cache key for the COMBINED **masked + custom-vertex** geometry raster
//! variant — a material that is BOTH glTF `MASK` AND carries a `wgsl_vertex`
//! displacement body.
//!
//! The union of [`super::masked_cache_key::ShaderCacheKeyGeometryMasked`] and
//! [`super::custom_vertex_cache_key::ShaderCacheKeyGeometryCustomVertex`]: it
//! carries BOTH the masked alpha info (so the fragment alpha-tests) AND the
//! custom-vertex displacement info (so the silhouette is displaced). Specialized
//! per `shader_id` like both parents — the assembled module embeds the
//! material's auto-generated `MaterialData` struct/loader + the author's vertex
//! body + (custom only) the author's alpha body.
//!
//! Like the masked + custom-vertex parents this is the non-instanced,
//! uniform-meta shape; instanced combos fall back to the plain custom-vertex /
//! masked / solid pipelines per the render-pass precedence.

use awsm_renderer_materials::MaterialShaderId;

use crate::{
    dynamic_materials::ShadingBase,
    render_passes::geometry::shader::{
        cache_key::DynamicVertexShaderInfo, masked_cache_key::DynamicAlphaShaderInfo,
    },
    render_passes::shader_cache_key::ShaderCacheKeyRenderPass,
    shaders::ShaderCacheKey,
};

/// Cache key for the combined masked + custom-vertex geometry raster shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyGeometryMaskedCustomVertex {
    /// Which material this combined variant renders for.
    pub shader_id: MaterialShaderId,
    /// Built-in shading family (selects the base-color alpha path) or `Custom`.
    pub base: ShadingBase,
    /// The displacement hook — auto-generated `MaterialData` struct + loader +
    /// the author's `wgsl_vertex` body. Emitted by the VERTEX stage; for a
    /// Custom material the fragment alpha path reuses the same struct/loader.
    pub dynamic_vertex: DynamicVertexShaderInfo,
    /// `Some` when the material is a dynamic (Custom) `MASK`: the author's
    /// alpha-only fragment + its texture helpers. `None` for built-in `MASK`
    /// (PBR/Unlit/Toon/Flipbook), which take the base-color alpha path. The
    /// struct/loader inside it are IGNORED here (the vertex emits them).
    pub dynamic_alpha: Option<DynamicAlphaShaderInfo>,
    /// Texture-pool array bindings the (masked) bind groups declare.
    pub texture_pool_arrays_len: u32,
    /// Texture-pool sampler bindings, same role.
    pub texture_pool_samplers_len: u32,
    /// MSAA sample count of the visibility buffer (`Some(4)`), or `None`. When
    /// multisampled the masked fragment emits per-sample cutout coverage.
    pub msaa_samples: Option<u32>,
}

impl From<ShaderCacheKeyGeometryMaskedCustomVertex> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyGeometryMaskedCustomVertex) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::GeometryMaskedCustomVertex(key))
    }
}
