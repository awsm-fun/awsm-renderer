//! Shader cache key for the display pass.

use crate::{
    post_process::ToneMapping, render_passes::shader_cache_key::ShaderCacheKeyRenderPass,
    shaders::ShaderCacheKey,
};

/// Cache key for display pass shaders.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyDisplay {
    pub tonemapping: ToneMapping,
}

impl From<ShaderCacheKeyDisplay> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyDisplay) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Display(key))
    }
}
