use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyDecalClassify;

impl From<ShaderCacheKeyDecalClassify> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyDecalClassify) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::DecalClassify(key))
    }
}
