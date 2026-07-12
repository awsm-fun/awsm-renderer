//! Bloom shader cache keys.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Which bloom pyramid step a [`ShaderCacheKeyBloomDownsample`] compiles.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BloomPyramidStep {
    /// Composite → pyramid mip 0 with the soft-knee threshold (13-tap ×2
    /// downsample variant).
    Prefilter,
    /// Plain 13-tap ×2 downsample, pyramid mip N-1 → mip N.
    Downsample,
    /// Progressive 9-tap tent accumulation,
    /// `up[N-1] = down[N-1] + scatter · tent9(mip N)`, coarsest → finest.
    Upsample,
}

/// Cache key for the bloom pyramid-step shaders (prefilter / downsample /
/// upsample). All three route through the shared
/// `ShaderCacheKeyRenderPass::BloomDownsample` variant — including the tent
/// upsample — so the cross-pass dispatch enums need no bloom-only churn; the
/// `step` field is what differentiates the compiled source.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyBloomDownsample {
    pub step: BloomPyramidStep,
}

impl From<ShaderCacheKeyBloomDownsample> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyBloomDownsample) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::BloomDownsample(key))
    }
}

/// Cache key for the bloom combine shader (tent-tap of the accumulated
/// up-pyramid mip 0 → full-res bloom). Format-only — no per-frame variation;
/// one shared pipeline.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyBloomCombine;

impl From<ShaderCacheKeyBloomCombine> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyBloomCombine) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::BloomCombine(key))
    }
}
