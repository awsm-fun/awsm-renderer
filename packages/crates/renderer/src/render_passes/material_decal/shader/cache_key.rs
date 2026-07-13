//! Shader cache key for the material decal compute pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the decal compute pipeline.
///
/// MSAA + texture-pool dimensions matter (sampled-binding signatures
/// differ). The visibility texture is only used to look up
/// `receive_decals`, so the classify-style multisampled-vs-single
/// distinction is needed.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialDecal {
    pub msaa_sample_count: Option<u32>,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    /// Divisor that packs/unpacks a decal's flat `texture_index` into the pool's
    /// `(array_index, layer_index)` — the device `max_texture_array_layers` (A.4).
    /// Device-constant in practice, so it adds no real variant; carried here so the
    /// compute template substitutes the exact stride the loader packs with.
    pub texture_pool_layers_per_array: u32,
    /// Depth convention (003).
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyMaterialDecal> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialDecal) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialDecal(key))
    }
}
