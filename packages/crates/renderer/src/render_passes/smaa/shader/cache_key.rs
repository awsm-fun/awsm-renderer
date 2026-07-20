//! SMAA shader cache keys.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Which SMAA step a [`ShaderCacheKeySmaa`] compiles.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmaaStep {
    /// Composite → edges texture (compressed-space luma contrast + local
    /// contrast adaptation).
    Edges,
    /// Edges → blend-weights texture (edge-segment search + analytic
    /// orthogonal-pattern area).
    Weights,
}

/// Cache key for the SMAA pre-pass shaders. Static sources — no per-config
/// variation; the `step` field selects which of the two compiles.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeySmaa {
    pub step: SmaaStep,
}

impl From<ShaderCacheKeySmaa> for ShaderCacheKey {
    fn from(key: ShaderCacheKeySmaa) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Smaa(key))
    }
}
