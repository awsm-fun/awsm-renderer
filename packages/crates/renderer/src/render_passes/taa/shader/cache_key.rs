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
