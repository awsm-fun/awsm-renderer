//! HZB compute pipelines.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::hzb::{
    bind_group::HzbBindGroups,
    shader::cache_key::{ShaderCacheKeyHzbReduce, ShaderCacheKeyHzbSeed},
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct HzbPipelines {
    pub seed_msaa: ComputePipelineKey,
    pub seed_single: ComputePipelineKey,
    pub reduce: ComputePipelineKey,
}

/// Descriptors for the 3 HZB compute pipelines (seed-msaa,
/// seed-single, reduce). Slot order matches
/// [`HzbPrewarmDescriptors::pipeline_cache_keys`] — index 0 = msaa,
/// 1 = single, 2 = reduce.
pub struct HzbPrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
}

impl HzbPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &HzbBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys())
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

    pub fn shader_cache_keys() -> Vec<ShaderCacheKey> {
        vec![
            ShaderCacheKey::from(ShaderCacheKeyHzbSeed {
                msaa_sample_count: Some(4),
            }),
            ShaderCacheKey::from(ShaderCacheKeyHzbSeed {
                msaa_sample_count: None,
            }),
            ShaderCacheKey::from(ShaderCacheKeyHzbReduce),
        ]
    }

    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &HzbBindGroups,
    ) -> Result<HzbPrewarmDescriptors> {
        let seed_msaa_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.seed_layout_key_msaa]),
        )?;
        let seed_single_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.seed_layout_key_single]),
        )?;
        let reduce_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.reduce_layout_key]),
        )?;

        let seed_msaa_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyHzbSeed {
                    msaa_sample_count: Some(4),
                },
            )
            .await?;
        let seed_single_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyHzbSeed {
                    msaa_sample_count: None,
                },
            )
            .await?;
        let reduce_shader = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyHzbReduce)
            .await?;

        Ok(HzbPrewarmDescriptors {
            pipeline_cache_keys: vec![
                ComputePipelineCacheKey::new(seed_msaa_shader, seed_msaa_layout),
                ComputePipelineCacheKey::new(seed_single_shader, seed_single_layout),
                ComputePipelineCacheKey::new(reduce_shader, reduce_layout),
            ],
        })
    }

    pub fn from_resolved(pipeline_keys: Vec<ComputePipelineKey>) -> Self {
        Self {
            seed_msaa: pipeline_keys[0],
            seed_single: pipeline_keys[1],
            reduce: pipeline_keys[2],
        }
    }
}
