use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::coverage::{
    bind_group::CoverageBindGroups, shader::cache_key::ShaderCacheKeyCoverage,
};
use crate::render_passes::RenderPassInitContext;

pub struct CoveragePipelines {
    pub compute: ComputePipelineKey,
}

impl CoveragePipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &CoverageBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let shader_key = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyCoverage {
                    multisampled: bind_groups.multisampled,
                },
            )
            .await?;
        let compute = ctx
            .pipelines
            .compute
            .get_key(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                ComputePipelineCacheKey::new(shader_key, pipeline_layout_key),
            )
            .await?;
        Ok(Self { compute })
    }
}
