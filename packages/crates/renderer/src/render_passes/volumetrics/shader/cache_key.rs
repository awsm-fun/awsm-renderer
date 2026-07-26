//! Volumetrics shader cache keys.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Which volumetrics stage a [`ShaderCacheKeyVolumetrics`] compiles.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumetricsStage {
    /// Per-froxel medium + in-scattered light.
    Inject,
    /// Per-column front-to-back accumulation of the injected volume.
    Integrate,
}

/// Cache key for the volumetrics compute shaders.
///
/// `reverse_z` rides the key because the shared shadow include is compiled
/// against the depth convention, not because the volume itself cares — the
/// froxel slice mapping is in view space either way.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyVolumetrics {
    pub stage: VolumetricsStage,
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyVolumetrics> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyVolumetrics) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Volumetrics(key))
    }
}
