use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::coverage::{
    bind_group::CoverageBindGroups, shader::cache_key::ShaderCacheKeyCoverage,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct CoveragePipelines {
    pub compute: ComputePipelineKey,
}

pub struct CoveragePrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
}

impl CoveragePipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &CoverageBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys(bind_groups))
            .await?;
        let descs = Self::build_descriptors(ctx, bind_groups).await?;
        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                descs.pipeline_cache_keys.clone(),
            )
            .await?;
        Ok(Self::from_resolved(pipeline_keys))
    }

    pub fn shader_cache_keys(bind_groups: &CoverageBindGroups) -> Vec<ShaderCacheKey> {
        vec![ShaderCacheKey::from(ShaderCacheKeyCoverage {
            multisampled: bind_groups.multisampled,
        })]
    }

    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &CoverageBindGroups,
    ) -> Result<CoveragePrewarmDescriptors> {
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
        Ok(CoveragePrewarmDescriptors {
            pipeline_cache_keys: vec![ComputePipelineCacheKey::new(
                shader_key,
                pipeline_layout_key,
            )],
        })
    }

    pub fn from_resolved(pipeline_keys: Vec<ComputePipelineKey>) -> Self {
        Self {
            compute: pipeline_keys[0],
        }
    }
}
