//! Bloom compute pipelines.
//!
//! Three pipelines over one shared bind-group layout: `prefilter` (composite →
//! mip 0 with soft-knee threshold), `downsample` (plain 13-tap pyramid step),
//! and `combine` (mip-sum upsample → full-res bloom). Self-contained: `new`
//! ensures its shader + pipeline cache keys directly rather than joining the
//! cross-renderer pool (bloom is cheap — 3 tiny compute shaders).

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::bloom::{
    bind_group::BloomBindGroups,
    shader::cache_key::{ShaderCacheKeyBloomCombine, ShaderCacheKeyBloomDownsample},
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct BloomPipelines {
    pub prefilter: ComputePipelineKey,
    pub downsample: ComputePipelineKey,
    pub combine: ComputePipelineKey,
}

impl BloomPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &BloomBindGroups,
    ) -> Result<Self> {
        // Warm the shader cache for all three variants.
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys())
            .await?;

        // Single shared pipeline layout — all three steps use the same
        // bind-group layout shape.
        let pipeline_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;

        let prefilter_shader = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyBloomDownsample { prefilter: true })
            .await?;
        let downsample_shader = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyBloomDownsample { prefilter: false })
            .await?;
        let combine_shader = ctx.shaders.get_key(ctx.gpu, ShaderCacheKeyBloomCombine).await?;

        let cache_keys = vec![
            ComputePipelineCacheKey::new(prefilter_shader, pipeline_layout),
            ComputePipelineCacheKey::new(downsample_shader, pipeline_layout),
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
            combine: pipeline_keys[2],
        })
    }

    /// Shader cache keys for the three bloom compute shaders.
    pub fn shader_cache_keys() -> Vec<ShaderCacheKey> {
        vec![
            ShaderCacheKey::from(ShaderCacheKeyBloomDownsample { prefilter: true }),
            ShaderCacheKey::from(ShaderCacheKeyBloomDownsample { prefilter: false }),
            ShaderCacheKey::from(ShaderCacheKeyBloomCombine),
        ]
    }
}
