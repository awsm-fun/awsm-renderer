//! Occlusion-cull compute pipeline.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::occlusion::{
    bind_group::OcclusionBindGroups, shader::cache_key::ShaderCacheKeyOcclusionCull,
};
use crate::render_passes::RenderPassInitContext;

pub struct OcclusionPipelines {
    pub cull: ComputePipelineKey,
}

impl OcclusionPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &OcclusionBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let shader_key = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyOcclusionCull)
            .await?;
        let cull = ctx
            .pipelines
            .compute
            .get_key(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                ComputePipelineCacheKey::new(shader_key, pipeline_layout_key),
            )
            .await?;
        Ok(Self { cull })
    }
}
