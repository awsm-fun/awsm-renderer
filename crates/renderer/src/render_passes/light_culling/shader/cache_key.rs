//! Shader cache key for the light culling pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for light culling shaders.
///
/// Only one field drives shader-template substitution:
/// - `slice_count` — number of view-space Z slices. Fixed at compile time so
///   the exponential mapping loop can constant-fold; bumping it forces a
///   recompile (rare — the renderer doesn't expose a per-scene knob).
///
/// Per-froxel index-buffer capacity is deliberately *not* part of this key:
/// the WGSL reads `cull_params.max_per_froxel_capacity` at runtime rather
/// than via template substitution, so the auto-grow path resizes buffers and
/// rewrites the uniform without recompiling — the generated source is
/// byte-for-byte identical across capacities.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyLightCulling {
    pub slice_count: u32,
}

impl From<ShaderCacheKeyLightCulling> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyLightCulling) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::LightCulling(key))
    }
}
