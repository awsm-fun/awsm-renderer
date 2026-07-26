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

/// How the effects pass renders atmospheric haze. Not two booleans: the
/// volumetric path REPLACES the analytic one (they describe the same medium,
/// so compiling both would double-count the air), which makes this a
/// three-way choice — and a three-way type is the way to make the invalid
/// fourth state unrepresentable.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtmospherePhase {
    /// No haze term compiled in at all.
    None,
    /// Closed-form per-pixel fog along the view ray. Cheap; no light shafts.
    Analytic,
    /// Sample the froxel scattering volume built by the volumetrics pass.
    Volumetric,
}

/// Cache key for effects pass shaders.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyEffects {
    pub smaa_anti_alias: bool,
    pub multisampled_geometry: bool,
    pub bloom_phase: BloomPhase,
    pub dof: bool,
    /// Atmospheric haze. Structural: `None` ⇒ no haze term compiled in at all,
    /// which is what "zero cost when off" has to mean. Colour/density/heights
    /// ride the `AtmosphereParams` uniform and never touch this key.
    pub atmosphere: AtmospherePhase,
    /// Depth convention (003).
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyEffects> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyEffects) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Effects(key))
    }
}
