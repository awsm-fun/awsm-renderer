//! Shader cache key for the global final_blend shader (the MSAA
//! edge-resolve flow's compositor; Priority 3 in
//! https://github.com/dakom/awsm-renderer/pull/99).

use crate::{
    dynamic_materials::BucketEntry, render_passes::shader_cache_key::ShaderCacheKeyRenderPass,
    shaders::ShaderCacheKey,
};

/// Cache key for the global final_blend compositor.
///
/// Keys on `(bucket_entries, color_format)` — `color_format` enters
/// because the storage texture binding declares the resolved render-
/// texture format; flipping HDR vs LDR requires a recompile.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialFinalBlend {
    pub bucket_entries: Vec<BucketEntry>,
    /// WGSL format string (e.g. `"rgba16float"` / `"rgba8unorm"`) for
    /// the opaque storage texture binding.
    pub color_format: String,
    /// SSR axis: when on, final_blend also resolves the per-pixel SSR
    /// reflection descriptor from the edge arms' per-sample sums and
    /// writes `reflection_descriptor_tex`.
    pub write_ssr_descriptor: bool,
}

impl From<ShaderCacheKeyMaterialFinalBlend> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialFinalBlend) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialFinalBlend(key))
    }
}
