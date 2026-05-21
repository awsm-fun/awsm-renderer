use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyDecalClassify {
    /// Adds the HZB texture binding + per-tile occlusion gate to
    /// the classify shader (plan §16.4.C). Only set when
    /// `features.gpu_culling` is on — the HZB texture is gated on
    /// that flag.
    pub hzb_enabled: bool,
}

impl From<ShaderCacheKeyDecalClassify> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyDecalClassify) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::DecalClassify(key))
    }
}
