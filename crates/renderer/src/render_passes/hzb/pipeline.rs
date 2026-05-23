//! HZB compute pipelines.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::hzb::{
    bind_group::HzbBindGroups,
    shader::cache_key::{ShaderCacheKeyHzbReduce, ShaderCacheKeyHzbSeed},
};
use crate::render_passes::RenderPassInitContext;

pub struct HzbPipelines {
    pub seed_msaa: ComputePipelineKey,
    pub seed_single: ComputePipelineKey,
    pub reduce: ComputePipelineKey,
}

impl HzbPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &HzbBindGroups,
    ) -> Result<Self> {
        // Pre-warm all 3 shader variants concurrently (seed-msaa,
        // seed-single, reduce). Pipeline assembly stays serial because
        // `ctx` is `&mut`.
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                [
                    ShaderCacheKeyHzbSeed {
                        msaa_sample_count: Some(4),
                    }
                    .into(),
                    ShaderCacheKeyHzbSeed {
                        msaa_sample_count: None,
                    }
                    .into(),
                    ShaderCacheKeyHzbReduce.into(),
                ],
            )
            .await?;
        let seed_msaa = build_seed(ctx, bind_groups, true).await?;
        let seed_single = build_seed(ctx, bind_groups, false).await?;
        let reduce = build_reduce(ctx, bind_groups).await?;
        Ok(Self {
            seed_msaa,
            seed_single,
            reduce,
        })
    }
}

async fn build_seed(
    ctx: &mut RenderPassInitContext<'_>,
    bind_groups: &HzbBindGroups,
    multisampled_geometry: bool,
) -> Result<ComputePipelineKey> {
    let layout_key = if multisampled_geometry {
        bind_groups.seed_layout_key_msaa
    } else {
        bind_groups.seed_layout_key_single
    };
    let pipeline_layout_key = ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![layout_key]),
    )?;
    let shader_key = ctx
        .shaders
        .get_key(
            ctx.gpu,
            ShaderCacheKeyHzbSeed {
                msaa_sample_count: if multisampled_geometry { Some(4) } else { None },
            },
        )
        .await?;
    Ok(ctx
        .pipelines
        .compute
        .get_key(
            ctx.gpu,
            ctx.shaders,
            ctx.pipeline_layouts,
            ComputePipelineCacheKey::new(shader_key, pipeline_layout_key),
        )
        .await?)
}

async fn build_reduce(
    ctx: &mut RenderPassInitContext<'_>,
    bind_groups: &HzbBindGroups,
) -> Result<ComputePipelineKey> {
    let pipeline_layout_key = ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![bind_groups.reduce_layout_key]),
    )?;
    let shader_key = ctx
        .shaders
        .get_key(ctx.gpu, ShaderCacheKeyHzbReduce)
        .await?;
    Ok(ctx
        .pipelines
        .compute
        .get_key(
            ctx.gpu,
            ctx.shaders,
            ctx.pipeline_layouts,
            ComputePipelineCacheKey::new(shader_key, pipeline_layout_key),
        )
        .await?)
}
