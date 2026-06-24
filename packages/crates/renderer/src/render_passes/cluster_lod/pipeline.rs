//! Cluster-LOD cut compute pipeline (Phase B, B.2).
//!
//! Creating this pipeline is the first on-device validation of
//! `cluster_cut.wgsl` (the GPU driver compiles + validates the WGSL here).

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::cluster_lod::{
    bind_group::ClusterCutBindGroups, shader::cache_key::ShaderCacheKeyClusterCut,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct ClusterLodPipelines {
    pub cut: ComputePipelineKey,
}

impl ClusterLodPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &ClusterCutBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys())
            .await?;
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let shader_key = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyClusterCut)
            .await?;
        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                vec![ComputePipelineCacheKey::new(shader_key, pipeline_layout_key)],
            )
            .await?;
        Ok(Self {
            cut: pipeline_keys[0],
        })
    }

    pub fn shader_cache_keys() -> Vec<ShaderCacheKey> {
        vec![ShaderCacheKey::from(ShaderCacheKeyClusterCut)]
    }
}
