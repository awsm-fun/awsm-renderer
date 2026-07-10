//! Cache keys for the occlusion compute shaders.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the occlusion-cull compute shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyOcclusionCull {
    /// Depth convention (003): flips the four coupled sites in lockstep with
    /// the HZB reduce op (init sentinel, nearest-corner reduce, footprint
    /// reduce, in-front compare) plus the clipped-corner bypass sentinel.
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyOcclusionCull> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyOcclusionCull) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::OcclusionCull(key))
    }
}

/// Cache key for the GPU instance-compaction shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyOcclusionCompaction {
    /// Whether to write a non-zero `IndirectDrawArgs.first_instance`
    /// (the per-mesh slot index) into the args buffer. Requires the
    /// `indirect-first-instance` WebGPU feature on the device — when
    /// the feature is absent (portable fallback path), the field
    /// must stay 0 or drawIndexedIndirect generates a validation
    /// error. The CPU passes the slot identity via bind-group dynamic
    /// offset instead.
    pub write_first_instance: bool,
}

impl From<ShaderCacheKeyOcclusionCompaction> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyOcclusionCompaction) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::OcclusionCompaction(key))
    }
}
