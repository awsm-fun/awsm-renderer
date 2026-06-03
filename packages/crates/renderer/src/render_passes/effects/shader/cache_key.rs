//! Shader cache key definitions for the effects pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Phase of multi-pass bloom processing
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BloomPhase {
    /// No bloom - other effects only
    None,
    /// First pass: extract bright pixels from composite, initial blur
    Extract,
    /// Middle passes: blur the previous result
    Blur,
    /// Final pass: blur and blend with original composite
    Blend,
}

/// Cache key for effects pass shaders.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyEffects {
    pub smaa_anti_alias: bool,
    pub multisampled_geometry: bool,
    pub bloom_phase: BloomPhase,
    pub dof: bool,
    pub ping_pong: bool,
}

impl From<ShaderCacheKeyEffects> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyEffects) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Effects(key))
    }
}
