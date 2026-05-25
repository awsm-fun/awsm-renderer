//! Shader cache keys for the opaque material pass.

use awsm_materials::MaterialShaderId;

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for opaque material shaders.
///
/// The opaque pass keys per `(MsaaConfig, mipmaps, shader_id)`. Each
/// variant lives in its own compute pipeline so the runtime `if
/// (shader_id == PBR) …` branch becomes a static `{% match shader_id %}`
/// template choice.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialOpaque {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub msaa_sample_count: Option<u32>,
    pub mipmaps: bool,
    pub shader_id: MaterialShaderId,
    /// Stable hash over the currently-registered dynamic-material set
    /// (sorted by shader_id, then `(name, layout_hash, wgsl_hash)` per
    /// entry).
    ///
    /// **Returns `0` when no dynamic materials are registered**, which
    /// is the stable empty-state sentinel — the cache key's hash is
    /// bit-identical to the pre-dynamic-material build, so first-party
    /// pipelines compile to the same WGSL they did before this feature
    /// shipped. Registering / unregistering a dynamic material changes
    /// `dispatch_hash`, invalidates affected pipelines on next render,
    /// and triggers a recompile.
    ///
    /// See `awsm_renderer::dynamic_materials::DynamicMaterials::dispatch_hash`
    /// for the hashing details.
    pub dispatch_hash: u64,
}

impl From<ShaderCacheKeyMaterialOpaque> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialOpaque) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialOpaque(key))
    }
}

/// Cache key for the opaque pass when no geometry is rendered.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialOpaqueEmpty {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub msaa_sample_count: Option<u32>,
}

impl From<ShaderCacheKeyMaterialOpaqueEmpty> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialOpaqueEmpty) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialOpaqueEmpty(key))
    }
}
