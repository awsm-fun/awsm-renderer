//! Shader cache key for the material decal compute pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the decal compute pipeline.
///
/// MSAA + texture-pool dimensions matter (sampled-binding signatures
/// differ). The visibility texture is only used to look up
/// `receive_decals`, so the classify-style multisampled-vs-single
/// distinction is needed.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialDecal {
    pub msaa_sample_count: Option<u32>,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
}

impl From<ShaderCacheKeyMaterialDecal> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialDecal) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialDecal(key))
    }
}
