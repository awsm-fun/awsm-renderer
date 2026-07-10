//! SSR min-Z pyramid shader cache keys.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the min-Z seed shader (depth → pyramid mip 0). The MSAA
/// mode matters because the depth texture's binding type changes — and it
/// must match the SSR trace's depth binding so the pyramid mirrors what the
/// trace reads.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeySsrMinzSeed {
    pub msaa_sample_count: Option<u32>,
    /// Depth convention (003).
    pub reverse_z: bool,
}

impl From<ShaderCacheKeySsrMinzSeed> for ShaderCacheKey {
    fn from(key: ShaderCacheKeySsrMinzSeed) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::SsrMinzSeed(key))
    }
}

/// Cache key for the min-Z reduce shader (mip N-1 → mip N).
/// No per-frame variation; one pipeline per depth convention.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeySsrMinzReduce {
    /// Depth convention (003).
    pub reverse_z: bool,
}

impl From<ShaderCacheKeySsrMinzReduce> for ShaderCacheKey {
    fn from(key: ShaderCacheKeySsrMinzReduce) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::SsrMinzReduce(key))
    }
}
