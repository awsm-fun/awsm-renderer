use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyCoverage {
    /// `true` when the visibility-data texture is multisampled
    /// (MSAA path). The compute shader uses `textureLoad` either
    /// way; this flag picks the matching `multisampled` flag on
    /// the bind-group layout entry.
    pub multisampled: bool,
}

impl From<ShaderCacheKeyCoverage> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyCoverage) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Coverage(key))
    }
}
