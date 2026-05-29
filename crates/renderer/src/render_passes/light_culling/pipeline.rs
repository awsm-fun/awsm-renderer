//! Compute pipeline for the light culling pass.
//!
//! Single shader module, two pipelines (`cs_main` Z-refine + `cs_tile`
//! side-plane cull) selected by entry point. The cull is MSAA-agnostic
//! (it doesn't sample the visibility or depth textures). The cache key
//! carries only `slice_count`: the per-froxel index-buffer capacity is a
//! *runtime* `cull_params` field, so auto-grow resizes buffers and
//! rewrites the uniform without recompiling.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::light_culling::{
    bind_group::LightCullingBindGroups, buffers::DEFAULT_SLICE_COUNT,
    shader::cache_key::ShaderCacheKeyLightCulling,
};
use crate::render_passes::RenderPassInitContext;

/// Compiled compute pipelines for the cull shader plus the `slice_count`
/// they were compiled against. Per-froxel capacity is deliberately not
/// tracked here — it's a runtime `cull_params` field that never triggers
/// a recompile (see the module doc and `set_max_per_froxel_capacity`).
pub struct LightCullingPipelines {
    /// Stage B (`cs_main`) — per-froxel Z-refine pipeline.
    pub pipeline_key: ComputePipelineKey,
    /// Stage A (`cs_tile`) — per-2D-tile side-plane cull pipeline.
    /// Compiled from the same shader module as `pipeline_key`, selected
    /// via the `cs_tile` entry point.
    pub tile_pipeline_key: ComputePipelineKey,
    pub slice_count: u32,
}

impl LightCullingPipelines {
    /// Builds the cull pipelines at the default `DEFAULT_SLICE_COUNT`.
    /// `LightCullingBuffers::new` is allocated at the matching slice
    /// count so the consumer / cull shaders agree on the WGSL constants.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &LightCullingBindGroups,
    ) -> Result<Self> {
        Self::build(ctx, bind_groups, DEFAULT_SLICE_COUNT).await
    }

    async fn build(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &LightCullingBindGroups,
        slice_count: u32,
    ) -> Result<Self> {
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.bind_group_layout_key]),
        )?;
        let shader_key = ctx
            .shaders
            .get_key(ctx.gpu, ShaderCacheKeyLightCulling { slice_count })
            .await?;
        let pipeline_key = ctx
            .pipelines
            .compute
            .get_key(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                ComputePipelineCacheKey::new(shader_key, pipeline_layout_key)
                    .with_entry_point("cs_main"),
            )
            .await?;
        let tile_pipeline_key = ctx
            .pipelines
            .compute
            .get_key(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                ComputePipelineCacheKey::new(shader_key, pipeline_layout_key)
                    .with_entry_point("cs_tile"),
            )
            .await?;
        Ok(Self {
            pipeline_key,
            tile_pipeline_key,
            slice_count,
        })
    }
}
