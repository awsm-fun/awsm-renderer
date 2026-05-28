//! Compute pipeline for the light culling pass.
//!
//! Single pipeline — the cull is MSAA-agnostic (it doesn't sample the
//! visibility or depth textures). The cache key carries
//! `(slice_count, max_per_froxel_capacity)`; auto-grow on overflow
//! recompiles the pipeline through `Self::rebuild`, which re-issues the
//! shader + pipeline cache key at the new capacity.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::light_culling::{
    bind_group::LightCullingBindGroups,
    buffers::{DEFAULT_MAX_PER_FROXEL_CAPACITY, DEFAULT_SLICE_COUNT},
    shader::cache_key::ShaderCacheKeyLightCulling,
};
use crate::render_passes::RenderPassInitContext;

/// Compiled compute pipeline for the cull shader plus the
/// `(slice_count, max_per_froxel_capacity)` the pipeline was compiled
/// against. The render path consults these to decide if a rebuild is
/// required after `set_max_per_froxel_capacity`.
pub struct LightCullingPipelines {
    pub pipeline_key: ComputePipelineKey,
    pub slice_count: u32,
    pub max_per_froxel_capacity: u32,
}

impl LightCullingPipelines {
    /// Builds the cull pipeline at the default
    /// (`DEFAULT_SLICE_COUNT`, `DEFAULT_MAX_PER_FROXEL_CAPACITY`).
    /// `LightCullingBuffers::new` is allocated at the matching pair so
    /// the consumer / cull shaders agree on the WGSL constants.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &LightCullingBindGroups,
    ) -> Result<Self> {
        Self::build(
            ctx,
            bind_groups,
            DEFAULT_SLICE_COUNT,
            DEFAULT_MAX_PER_FROXEL_CAPACITY,
        )
        .await
    }

    /// Rebuild the pipeline at a new capacity. Invoked after
    /// `LightCullingBuffers::set_max_per_froxel_capacity` so the shader
    /// constants match the new buffer layout. Cheap on cache hit — the
    /// shader + pipeline caches dedupe on the cache key.
    pub async fn rebuild(
        &mut self,
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        bind_group_layouts: &mut crate::bind_group_layout::BindGroupLayouts,
        pipeline_layouts: &mut crate::pipeline_layouts::PipelineLayouts,
        shaders: &mut crate::shaders::Shaders,
        pipelines: &mut crate::pipelines::Pipelines,
        bind_groups: &LightCullingBindGroups,
        slice_count: u32,
        max_per_froxel_capacity: u32,
    ) -> Result<()> {
        if slice_count == self.slice_count
            && max_per_froxel_capacity == self.max_per_froxel_capacity
        {
            return Ok(());
        }
        let pipeline_layout_key = pipeline_layouts.get_key(
            gpu,
            bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.bind_group_layout_key]),
        )?;
        let shader_key = shaders
            .get_key(
                gpu,
                ShaderCacheKeyLightCulling {
                    slice_count,
                    max_per_froxel_capacity,
                },
            )
            .await?;
        let pipeline_key = pipelines
            .compute
            .get_key(
                gpu,
                shaders,
                pipeline_layouts,
                ComputePipelineCacheKey::new(shader_key, pipeline_layout_key),
            )
            .await?;
        self.pipeline_key = pipeline_key;
        self.slice_count = slice_count;
        self.max_per_froxel_capacity = max_per_froxel_capacity;
        Ok(())
    }

    async fn build(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &LightCullingBindGroups,
        slice_count: u32,
        max_per_froxel_capacity: u32,
    ) -> Result<Self> {
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.bind_group_layout_key]),
        )?;
        let shader_key = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyLightCulling {
                    slice_count,
                    max_per_froxel_capacity,
                },
            )
            .await?;
        let pipeline_key = ctx
            .pipelines
            .compute
            .get_key(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                ComputePipelineCacheKey::new(shader_key, pipeline_layout_key),
            )
            .await?;
        Ok(Self {
            pipeline_key,
            slice_count,
            max_per_froxel_capacity,
        })
    }
}
