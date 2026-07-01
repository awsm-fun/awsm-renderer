//! Shader cache key for the material prep compute pass (Plan B,
//! docs/plans/deferred-shared-prep-pass.md).

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the prep compute pipeline.
///
/// `msaa_sample_count` selects the visibility-texture binding type
/// (`texture_multisampled_2d` vs `texture_2d`) — like the classify pass, prep
/// reads sample 0 either way, but the declared type must match. The prep pass is
/// unconditional, so there's no `enabled` field here.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialPrep {
    pub msaa_sample_count: Option<u32>,
    /// `K` — the clamped per-pixel shadow-caster cap (`PrepPassConfig::clamped_k`).
    /// Threaded into the key so the prep pipeline varies with K (the shadow
    /// loop's slot clamp + the packed-layer count derive from it).
    pub max_shadow_casters: u32,
    /// Global SSCS enable (`ShadowsConfig::sscs_enabled`). Folded into the shadow
    /// module's compile-time `sscs_available` gate — when `false` the `apply_sscs`
    /// body is compiled out (zero cost, the default). Re-keys on toggle.
    pub sscs_enabled: bool,
    /// Global SSCS ray-march step count (`ShadowsConfig::sscs_step_count`, ≥1),
    /// baked as the `apply_sscs` loop bound (compile-time constant). Re-keys on change.
    pub sscs_step_count: u32,
}

impl From<ShaderCacheKeyMaterialPrep> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialPrep) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialPrep(key))
    }
}

/// Cache key for the OPTIONAL shadow-visibility denoise blur compute pipelines
/// (`cs_blur_h` / `cs_blur_v`). `msaa_sample_count` selects the depth binding
/// type (multisampled vs not); `max_shadow_casters` sets the packed-layer loop
/// count (`ceil(K/4)`), matching the prep output it blurs.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyShadowBlur {
    pub msaa_sample_count: Option<u32>,
    pub max_shadow_casters: u32,
}

impl From<ShaderCacheKeyShadowBlur> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyShadowBlur) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::ShadowBlur(key))
    }
}
