//! Shader cache key for the light culling pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for light culling shaders.
///
/// Two fields drive shader-template substitution:
/// - `slice_count` — number of view-space Z slices. Fixed at compile time so
///   the exponential mapping loop can constant-fold; bumping it forces a
///   recompile (rare — the renderer doesn't expose a per-scene knob).
/// - `max_per_froxel_capacity` — index-buffer budget per froxel. The
///   auto-grow path bumps this on observed overflow, which recompiles the
///   pipeline; steady-state scenes hit a stable budget after at most one
///   compile.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyLightCulling {
    pub slice_count: u32,
    pub max_per_froxel_capacity: u32,
}

impl From<ShaderCacheKeyLightCulling> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyLightCulling) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::LightCulling(key))
    }
}
