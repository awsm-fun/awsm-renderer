//! Cache keys for the occlusion compute shaders.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the occlusion-cull compute shader. Currently no
/// per-frame variation; one shared pipeline.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyOcclusionCull;

impl From<ShaderCacheKeyOcclusionCull> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyOcclusionCull) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::OcclusionCull(key))
    }
}

/// Cache key for the GPU instance-compaction shader (§16.7 Phase 2 +
/// §16.8 infrastructure).
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyOcclusionCompaction;

impl From<ShaderCacheKeyOcclusionCompaction> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyOcclusionCompaction) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::OcclusionCompaction(key))
    }
}
