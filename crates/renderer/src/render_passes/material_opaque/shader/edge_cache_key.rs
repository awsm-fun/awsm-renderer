//! Shader cache keys for the per-shader-id MSAA edge-resolve shaders +
//! the global skybox_edge_resolve + final_blend shaders (Priority 3 in
//! docs/plans/more-optimizations.md).

use awsm_materials::MaterialShaderId;

use crate::{
    dynamic_materials::BucketEntry,
    render_passes::{
        material_opaque::shader::cache_key::DynamicShaderInfo,
        shader_cache_key::ShaderCacheKeyRenderPass,
    },
    shaders::ShaderCacheKey,
};

/// Cache key for the per-shader-id edge_resolve compute shader.
///
/// Keys per `(shader_id, mipmap, dispatch_hash, bucket_entries)` —
/// MSAA isn't a key axis because edge_resolve only ever runs in the
/// multisampled context. The dispatch_hash + bucket_entries pair
/// mirrors the primary opaque key so dynamic-material drift triggers
/// a recompile in lockstep.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialEdgeResolve {
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub mipmaps: bool,
    pub shader_id: MaterialShaderId,
    /// Same `dispatch_hash` semantics as the primary opaque cache
    /// key — stable empty-state sentinel of `0`.
    pub dispatch_hash: u64,
    pub dynamic_shader: Option<DynamicShaderInfo>,
    pub bucket_entries: Vec<BucketEntry>,
    /// Bucket index this shader_id occupies inside `bucket_entries`.
    /// Threaded through so the templated edge_resolve shader can hard-
    /// code its `{{ bucket_index }}u` slot-match in the slot_map scan.
    pub bucket_index: u32,
}

impl From<ShaderCacheKeyMaterialEdgeResolve> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialEdgeResolve) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialEdgeResolve(key))
    }
}

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
