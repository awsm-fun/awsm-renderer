//! Shader cache key for the light culling pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for light culling shaders.
///
/// Fields driving shader-template substitution:
/// - `slice_count` — number of view-space Z slices. Fixed at compile time so
///   the exponential mapping loop can constant-fold; bumping it forces a
///   recompile (rare — the renderer doesn't expose a per-scene knob).
/// - `reverse_z` — the depth convention. The tile unproject helpers anchor a
///   corner ray at the NEAR plane's NDC z, which is 0.0 forward / 1.0
///   reverse. Unprojecting z=0 under infinite-reverse is the far plane at
///   infinity (`inv_proj * clip` has w→0), which NaN'd every tile's side
///   planes and culled ALL punctual lights.
///
/// Per-froxel index-buffer capacity is deliberately *not* part of this key:
/// the WGSL reads `cull_params.max_per_froxel_capacity` at runtime rather
/// than via template substitution, so the auto-grow path resizes buffers and
/// rewrites the uniform without recompiling — the generated source is
/// byte-for-byte identical across capacities.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyLightCulling {
    pub slice_count: u32,
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyLightCulling> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyLightCulling) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::LightCulling(key))
    }
}
