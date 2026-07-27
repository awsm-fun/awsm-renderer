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
///
/// `temporal` is a genuine structural axis, not a convenience: it adds the
/// history texture and its sampler to group 0's LAYOUT, which changes the
/// pipeline layout, so the two variants cannot share a pipeline. It also
/// changes what `inject` does — jitter the sample point and blend against a
/// reprojected history, instead of writing one unjittered centre sample.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyVolumetrics {
    pub stage: VolumetricsStage,
    pub reverse_z: bool,
    pub temporal: bool,
}

impl From<ShaderCacheKeyVolumetrics> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyVolumetrics) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Volumetrics(key))
    }
}
