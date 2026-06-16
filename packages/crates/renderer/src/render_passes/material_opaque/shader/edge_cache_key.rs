//! Shader cache keys for the per-shader-id MSAA edge-resolve shaders +
//! the global skybox_edge_resolve + final_blend shaders (Priority 3 in
//! https://github.com/dakom/awsm-renderer/pull/99).

use crate::{
    dynamic_materials::BucketEntry, render_passes::shader_cache_key::ShaderCacheKeyRenderPass,
    shaders::ShaderCacheKey,
};

/// Cache key for the global skybox_edge_resolve shader.
///
/// Keys on `bucket_entries` only — the shader doesn't have any
/// shader_id specialization; the bucket list flows in because the
/// `EdgeBuffers` / `EdgeBufferLayout` structs are templated against
/// it (one `args_<name>_edge` field per bucket).
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialSkyboxEdgeResolve {
    pub bucket_entries: Vec<BucketEntry>,
}

impl From<ShaderCacheKeyMaterialSkyboxEdgeResolve> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialSkyboxEdgeResolve) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialSkyboxEdgeResolve(key))
    }
}

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
}

impl From<ShaderCacheKeyMaterialFinalBlend> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialFinalBlend) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialFinalBlend(key))
    }
}
