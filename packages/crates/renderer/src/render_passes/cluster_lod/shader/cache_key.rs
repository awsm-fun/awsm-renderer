//! Cache key for the cluster-LOD cut compute shader (Phase B, B.2).

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for the cluster-cut compute shader. The per-instance camera/params
/// ride in a uniform buffer, so the only variant axis is `paging`: the dynamic
/// page-pool (Gap B) variant binds a `resident` table and culls absent clusters.
/// Default (`false`) is the shipped single-pipeline cut — byte-identical.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyClusterCut {
    /// Build the `cluster_paging` variant (binds `resident`, skips absent clusters).
    pub paging: bool,
}

impl From<ShaderCacheKeyClusterCut> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyClusterCut) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::ClusterCut(key))
    }
}

/// Cache key for the cluster-compaction compute shader. One shared pipeline.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyClusterCompaction;

impl From<ShaderCacheKeyClusterCompaction> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyClusterCompaction) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::ClusterCompaction(key))
    }
}
