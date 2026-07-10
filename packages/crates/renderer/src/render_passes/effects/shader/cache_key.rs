//! Shader cache key definitions for the effects pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Bloom participation of the effects pass. The wide glow itself is built by
/// the dedicated mip-pyramid `BloomRenderPass`; the effects pass either blends
/// that pre-built `bloom_tex` over the scene (`Blend`) or skips bloom entirely
/// (`None`). The old in-pass Extract/Blur phases (and their ping-pong axis)
/// were removed with the migration to the dedicated pass.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BloomPhase {
    /// No bloom - other effects only
    None,
    /// Blend the pre-built bloom texture with the original composite
    Blend,
}

/// Cache key for effects pass shaders.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyEffects {
    pub smaa_anti_alias: bool,
    pub multisampled_geometry: bool,
    pub bloom_phase: BloomPhase,
    pub dof: bool,
    /// Depth convention (003).
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyEffects> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyEffects) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Effects(key))
    }
}
