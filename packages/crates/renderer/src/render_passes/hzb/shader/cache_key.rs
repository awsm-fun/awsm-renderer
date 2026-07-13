//! HZB shader cache keys.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the HZB seed shader (depth → hzb mip 0). The MSAA
/// mode matters because the depth texture's binding type changes.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyHzbSeed {
    pub msaa_sample_count: Option<u32>,
    /// Depth convention (003): forward-Z stores the MAX (farthest) depth per
    /// texel; reverse-Z stores the MIN — "farthest" flips with the mapping.
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyHzbSeed> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyHzbSeed) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::HzbSeed(key))
    }
}

/// Cache key for the HZB reduce shader (mip N-1 → mip N).
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyHzbReduce {
    /// Depth convention (003) — must match the seed's flip (max↔min).
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyHzbReduce> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyHzbReduce) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::HzbReduce(key))
    }
}
