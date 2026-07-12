//! Bloom compute pipelines.
//!
//! Four pipelines: `prefilter` (composite → mip 0 with soft-knee threshold),
//! `downsample` (plain 13-tap pyramid step) and `combine` (tent-tap of the
//! accumulated up-pyramid → full-res bloom) share one bind-group layout;
//! `upsample` (progressive 9-tap tent accumulation) has its own layout with a
//! second sampled texture. Self-contained: `new` ensures its shader +
//! pipeline cache keys directly rather than joining the cross-renderer pool
//! (bloom is cheap — 4 tiny compute shaders).

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::bloom::{
    bind_group::BloomBindGroups,
    shader::cache_key::{
        BloomPyramidStep, ShaderCacheKeyBloomCombine, ShaderCacheKeyBloomDownsample,
    },
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct BloomPipelines {
    pub prefilter: ComputePipelineKey,
    pub downsample: ComputePipelineKey,
    pub upsample: ComputePipelineKey,
    pub combine: ComputePipelineKey,
}

impl BloomPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &BloomBindGroups,
    ) -> Result<Self> {
        // Warm the shader cache for all four variants.
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys())
            .await?;

        // Prefilter / downsample / combine share one bind-group layout; the
        // upsample has its own (extra sampled texture for the accumulation
        // base).
        let pipeline_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let upsample_pipeline_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.upsample_layout_key]),
        )?;

        let prefilter_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyBloomDownsample {
                    step: BloomPyramidStep::Prefilter,
                },
            )
            .await?;
        let downsample_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyBloomDownsample {
                    step: BloomPyramidStep::Downsample,
                },
            )
            .await?;
        let upsample_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyBloomDownsample {
                    step: BloomPyramidStep::Upsample,
                },
            )
            .await?;
        let combine_shader = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyBloomCombine)
            .await?;

        let cache_keys = vec![
            ComputePipelineCacheKey::new(prefilter_shader, pipeline_layout),
            ComputePipelineCacheKey::new(downsample_shader, pipeline_layout),
            ComputePipelineCacheKey::new(upsample_shader, upsample_pipeline_layout),
            ComputePipelineCacheKey::new(combine_shader, pipeline_layout),
        ];

        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;

        Ok(Self {
            prefilter: pipeline_keys[0],
            downsample: pipeline_keys[1],
            upsample: pipeline_keys[2],
            combine: pipeline_keys[3],
        })
    }

    /// Shader cache keys for the four bloom compute shaders.
    pub fn shader_cache_keys() -> Vec<ShaderCacheKey> {
        vec![
            ShaderCacheKey::from(ShaderCacheKeyBloomDownsample {
                step: BloomPyramidStep::Prefilter,
            }),
            ShaderCacheKey::from(ShaderCacheKeyBloomDownsample {
                step: BloomPyramidStep::Downsample,
            }),
            ShaderCacheKey::from(ShaderCacheKeyBloomDownsample {
                step: BloomPyramidStep::Upsample,
            }),
            ShaderCacheKey::from(ShaderCacheKeyBloomCombine),
        ]
    }
}
