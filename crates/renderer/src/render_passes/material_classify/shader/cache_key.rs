//! Shader cache key for the material classify compute pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the classify compute pipeline. MSAA matters because
/// the visibility texture is sampled either single- or multisampled;
/// the classify shader only reads sample 0 either way, but the
/// declared binding type has to match.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialClassify {
    pub msaa_sample_count: Option<u32>,
}

impl From<ShaderCacheKeyMaterialClassify> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialClassify) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialClassify(key))
    }
}
