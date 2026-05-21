use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_decal::classify::{
    bind_group::DecalClassifyBindGroups, shader::cache_key::ShaderCacheKeyDecalClassify,
};
use crate::render_passes::RenderPassInitContext;

pub struct DecalClassifyPipelines {
    pub cull: ComputePipelineKey,
}

impl DecalClassifyPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &DecalClassifyBindGroups,
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
                ShaderCacheKeyDecalClassify {
                    hzb_enabled: bind_groups.hzb_enabled,
                },
            )
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
