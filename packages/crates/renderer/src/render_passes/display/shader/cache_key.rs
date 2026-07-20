//! Shader cache key for the display pass.

use crate::{
    post_process::ToneMapping, render_passes::shader_cache_key::ShaderCacheKeyRenderPass,
    shaders::ShaderCacheKey,
};

/// Cache key for display pass shaders.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyDisplay {
    pub tonemapping: ToneMapping,
    /// True when `render_scale != 1.0`: the fragment resamples the
    /// (larger) composite down to the swap-chain with a manual bilinear
    /// instead of the 1:1 `textureLoad` (which stays byte-identical for
    /// the default scale).
    pub supersample: bool,
}

impl From<ShaderCacheKeyDisplay> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyDisplay) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Display(key))
    }
}
