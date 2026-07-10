//! SSR min-Z pyramid compute pipelines.
//!
//! Self-contained (like bloom / the SSR trace): `new` ensures its own shader
//! and pipeline cache keys directly rather than joining the cross-renderer
//! pool. Two pipelines: `seed` (depth to mip 0) over the seed layout, and
//! `reduce` (mip N-1 to mip N) over the reduce layout. We drop the occlusion
//! HZB's MSAA lazy-pool machinery: the seed is compiled for the single live-AA
//! variant, matching the SSR trace's depth binding.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::ssr_minz::{
    bind_group::SsrMinzBindGroups,
    shader::cache_key::{ShaderCacheKeySsrMinzReduce, ShaderCacheKeySsrMinzSeed},
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct SsrMinzPipelines {
    pub seed: ComputePipelineKey,
    pub reduce: ComputePipelineKey,
}

impl SsrMinzPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &SsrMinzBindGroups,
    ) -> Result<Self> {
        let seed_msaa = Self::seed_msaa(ctx.anti_aliasing);
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys(ctx.anti_aliasing))
            .await?;

        let seed_pipeline_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.seed_layout_key]),
        )?;
        let reduce_pipeline_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.reduce_layout_key]),
        )?;

        let seed_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeySsrMinzSeed {
                    msaa_sample_count: seed_msaa,
                },
            )
            .await?;
        let reduce_shader = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeySsrMinzReduce)
            .await?;

        let cache_keys = vec![
            ComputePipelineCacheKey::new(seed_shader, seed_pipeline_layout),
            ComputePipelineCacheKey::new(reduce_shader, reduce_pipeline_layout),
        ];

        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;

        Ok(Self {
            seed: pipeline_keys[0],
            reduce: pipeline_keys[1],
        })
    }

    fn seed_msaa(anti_aliasing: &crate::anti_alias::AntiAliasing) -> Option<u32> {
        match anti_aliasing.msaa_sample_count {
            Some(4) => Some(4),
            _ => None,
        }
    }

    pub fn shader_cache_keys(
        anti_aliasing: &crate::anti_alias::AntiAliasing,
    ) -> Vec<ShaderCacheKey> {
        vec![
            ShaderCacheKey::from(ShaderCacheKeySsrMinzSeed {
                msaa_sample_count: Self::seed_msaa(anti_aliasing),
            }),
            ShaderCacheKey::from(ShaderCacheKeySsrMinzReduce),
        ]
    }
}
