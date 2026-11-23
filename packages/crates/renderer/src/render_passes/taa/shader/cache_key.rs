//! TAA resolve shader cache key.
//!
//! The key is the permutation identity: only what changes the compiled WGSL
//! text lives here. Every tuning value (clip box, disocclusion slack, history
//! cap, motion alpha, debug view, reset) is a live uniform in `TaaParams` and
//! never recompiles.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the TAA resolve (`taa_wgsl/resolve.wgsl`).
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyTaa {
    /// Depth convention (003): selects the sky test and which raw depth is
    /// "nearer" for the velocity dilation and the disocclusion footprint.
    pub reverse_z: bool,
    /// Binds the reactive mask (plan phase 3) and takes its 3x3 max into the
    /// blend weight. Without it the shader binds and reads nothing extra.
    pub reactive: bool,
}

impl From<ShaderCacheKeyTaa> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyTaa) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Taa(key))
    }
}

/// Cache key for the reactive seed (`taa_wgsl/reactive_seed.wgsl`), built
/// only under TAA.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyTaaReactiveSeed {
    /// Also bind the SSR reflection descriptor and add `reflectivity *
    /// k_ssr` (03 §5.3). Used only while SSR runs with `reactive_ssr > 0`.
    pub ssr: bool,
}

impl From<ShaderCacheKeyTaaReactiveSeed> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyTaaReactiveSeed) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::TaaReactiveSeed(key))
    }
}
