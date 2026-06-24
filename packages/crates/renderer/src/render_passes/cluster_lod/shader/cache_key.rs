//! Cache key for the cluster-LOD cut compute shader (Phase B, B.2).

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the cluster-cut compute shader. No per-frame variation — one
/// shared pipeline (the per-instance camera/params ride in a uniform buffer).
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyClusterCut;

impl From<ShaderCacheKeyClusterCut> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyClusterCut) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::ClusterCut(key))
    }
}
