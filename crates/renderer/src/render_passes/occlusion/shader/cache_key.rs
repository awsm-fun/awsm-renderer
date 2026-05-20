//! Cache keys for the occlusion-cull shader.

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
