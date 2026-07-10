use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyDecalClassify {
    /// Adds the HZB texture binding + per-tile occlusion gate to
    /// the classify shader. Only set when `features.gpu_culling` is
    /// on — the HZB texture is gated on that flag.
    pub hzb_enabled: bool,
    /// Depth convention (003). Flips the HZB occlusion gate: under
    /// reverse-Z "closest" is the numerical MAX corner depth and the
    /// HZB stores the min-reduced (farthest) occluder bound, so the
    /// drop test inverts. Without this axis the forward-Z gate ran
    /// under reverse-Z and dropped every decal whose footprint
    /// touched the sky (hzb min = 0.0 clear) — i.e. all of them.
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyDecalClassify> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyDecalClassify) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::DecalClassify(key))
    }
}
