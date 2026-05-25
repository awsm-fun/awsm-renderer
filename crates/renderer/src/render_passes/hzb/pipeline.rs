//! HZB compute pipelines.

use crate::error::Result;
use crate::pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey};
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

impl HzbPipelines {
    /// Builds all three HZB pipelines (seed-msaa, seed-single, reduce)
    /// concurrently via batched `Shaders::ensure_keys` +
    /// `ComputePipelines::ensure_keys`.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &HzbBindGroups,
    ) -> Result<Self> {
        // Resolve pipeline layouts first — no compile cost; pure
        // hash-key registration.
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

        // Batch 1: 3 shader compiles in parallel.
        let shader_keys = [
            ShaderCacheKey::from(ShaderCacheKeyHzbSeed {
                msaa_sample_count: Some(4),
            }),
            ShaderCacheKey::from(ShaderCacheKeyHzbSeed {
                msaa_sample_count: None,
            }),
            ShaderCacheKey::from(ShaderCacheKeyHzbReduce),
        ];
        ctx.shaders
            .ensure_keys(ctx.gpu, shader_keys.iter().cloned())
            .await?;

        // Resolve shader keys (cache hits) + build compute pipeline
        // cache keys.
        let pairs: [(ShaderCacheKey, PipelineLayoutKey); 3] = [
            (shader_keys[0].clone(), seed_msaa_layout),
            (shader_keys[1].clone(), seed_single_layout),
            (shader_keys[2].clone(), reduce_layout),
        ];
        let mut pipeline_cache_keys: Vec<ComputePipelineCacheKey> = Vec::with_capacity(3);
        for (shader_cache, layout_key) in &pairs {
            let shader_key = ctx.shaders.get_key(ctx.gpu, shader_cache.clone()).await?;
            pipeline_cache_keys.push(ComputePipelineCacheKey::new(shader_key, *layout_key));
        }

        // Batch 2: 3 compute pipelines in parallel.
        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                pipeline_cache_keys,
            )
            .await?;

        Ok(Self {
            seed_msaa: pipeline_keys[0],
            seed_single: pipeline_keys[1],
            reduce: pipeline_keys[2],
        })
    }
}
