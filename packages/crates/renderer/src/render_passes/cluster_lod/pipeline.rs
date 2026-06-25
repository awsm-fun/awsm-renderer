//! Cluster-LOD cut compute pipeline (Phase B, B.2).
//!
//! Creating this pipeline is the first on-device validation of
//! `cluster_cut.wgsl` (the GPU driver compiles + validates the WGSL here).

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::cluster_lod::{
    bind_group::{ClusterCompactionBindGroups, ClusterCutBindGroups},
    shader::cache_key::{ShaderCacheKeyClusterCompaction, ShaderCacheKeyClusterCut},
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct ClusterLodPipelines {
    pub cut: ComputePipelineKey,
    pub compaction: ComputePipelineKey,
}

impl ClusterLodPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        cut_bg: &ClusterCutBindGroups,
        compaction_bg: &ClusterCompactionBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys())
            .await?;
        let cut_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![cut_bg.layout_key]),
        )?;
        let compaction_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![compaction_bg.layout_key]),
        )?;
        let cut_shader = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyClusterCut { paging: false })
            .await?;
        let compaction_shader = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyClusterCompaction)
            .await?;
        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                vec![
                    ComputePipelineCacheKey::new(cut_shader, cut_layout),
                    ComputePipelineCacheKey::new(compaction_shader, compaction_layout),
                ],
            )
            .await?;
        Ok(Self {
            cut: pipeline_keys[0],
            compaction: pipeline_keys[1],
        })
    }

    pub fn shader_cache_keys() -> Vec<ShaderCacheKey> {
        vec![
            ShaderCacheKey::from(ShaderCacheKeyClusterCut { paging: false }),
            ShaderCacheKey::from(ShaderCacheKeyClusterCompaction),
        ]
    }
}
