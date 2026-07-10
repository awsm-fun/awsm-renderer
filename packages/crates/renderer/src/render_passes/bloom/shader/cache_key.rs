//! Bloom shader cache keys.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the bloom down-sample shader. `prefilter` selects the
/// prefilter variant (composite → mip 0 with soft-knee threshold) vs. the
/// plain 13-tap pyramid downsample.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyBloomDownsample {
    pub prefilter: bool,
}

impl From<ShaderCacheKeyBloomDownsample> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyBloomDownsample) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::BloomDownsample(key))
    }
}

/// Cache key for the bloom combine shader (mip-sum upsample → full-res bloom).
/// Format-only — no per-frame variation; one shared pipeline.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyBloomCombine;

impl From<ShaderCacheKeyBloomCombine> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyBloomCombine) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::BloomCombine(key))
    }
}
