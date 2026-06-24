//! Shader cache key for the COMBINED **masked + custom-vertex** shadow caster —
//! a material that is BOTH glTF `MASK` (cutout shadow) AND carries a
//! `wgsl_vertex` displacement body.
//!
//! The union of [`super::masked_cache_key::ShaderCacheKeyShadowMasked`] and
//! [`super::custom_vertex_cache_key::ShaderCacheKeyShadowCustomVertex`]: the
//! depth-only shadow VERTEX runs the `custom_displace_vertex` hook (so the cutout
//! silhouette is DISPLACED — glued to the lit geometry), while the masked
//! FRAGMENT alpha-tests (so the displaced shadow is also a cutout). Reuses the
//! masked-shadow WGSL with `has_custom_vertex` flipped on; no new WGSL file.
//!
//! Specialized per `shader_id` like both parents. The shadow atlas is
//! single-sampled (no MSAA). Non-instanced today (matching the masked +
//! custom-vertex shadow parents).

use awsm_renderer_materials::MaterialShaderId;

use crate::{
    dynamic_materials::ShadingBase,
    render_passes::geometry::shader::{
        cache_key::DynamicVertexShaderInfo, masked_cache_key::DynamicAlphaShaderInfo,
    },
    shaders::ShaderCacheKey,
};

/// Cache key for the combined masked + custom-vertex shadow-generation shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyShadowMaskedCustomVertex {
    /// Which material this combined shadow variant renders for.
    pub shader_id: MaterialShaderId,
    /// Built-in shading family (selects the base-color alpha path) or `Custom`.
    pub base: ShadingBase,
    /// The displacement hook — auto-generated `MaterialData` struct + loader +
    /// the author's `wgsl_vertex` body. Emitted by the VERTEX stage; for a Custom
    /// material the fragment alpha path reuses the same struct/loader.
    pub dynamic_vertex: DynamicVertexShaderInfo,
    /// `Some` when the material is a dynamic (Custom) `MASK`: the author's
    /// alpha-only fragment + its texture helpers. `None` for built-in `MASK`. The
    /// struct/loader inside it are IGNORED here (the vertex emits them).
    pub dynamic_alpha: Option<DynamicAlphaShaderInfo>,
    /// Texture-pool array bindings the (masked-shadow) bind groups declare.
    pub texture_pool_arrays_len: u32,
    /// Texture-pool comparison/filter sampler bindings, same role.
    pub texture_pool_samplers_len: u32,
}

impl From<ShaderCacheKeyShadowMaskedCustomVertex> for ShaderCacheKey {
    fn from(value: ShaderCacheKeyShadowMaskedCustomVertex) -> Self {
        ShaderCacheKey::ShadowMaskedCustomVertex(value)
    }
}
