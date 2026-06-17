//! Shader cache key for the material prep compute pass (Plan B,
//! docs/plans/deferred-shared-prep-pass.md).

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the prep compute pipeline.
///
/// `msaa_sample_count` selects the visibility-texture binding type
/// (`texture_multisampled_2d` vs `texture_2d`) — like the classify pass, prep
/// reads sample 0 either way, but the declared type must match. The prep pass is
/// only created/dispatched when `PrepPassConfig.enabled`, so there's no `enabled`
/// field here — disabling the feature simply skips pipeline creation.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialPrep {
    pub msaa_sample_count: Option<u32>,
}

impl From<ShaderCacheKeyMaterialPrep> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialPrep) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialPrep(key))
    }
}
