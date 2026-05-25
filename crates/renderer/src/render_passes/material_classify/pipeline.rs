//! Compute pipeline for the material classify pass.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_classify::{
    bind_group::MaterialClassifyBindGroups, shader::cache_key::ShaderCacheKeyMaterialClassify,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

/// One compute pipeline per MSAA mode. The classify shader only
/// touches sample 0 of the visibility texture, but WGSL still has
/// distinct `texture_multisampled_2d<u32>` / `texture_2d<u32>` types
/// so the pipeline layout has to match.
pub struct MaterialClassifyPipelines {
    pub multisampled_pipeline_key: ComputePipelineKey,
    pub singlesampled_pipeline_key: ComputePipelineKey,
}

impl MaterialClassifyPipelines {
    /// Builds both MSAA variants concurrently via batched
    /// `Shaders::ensure_keys` + `ComputePipelines::ensure_keys`.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialClassifyBindGroups,
    ) -> Result<Self> {
        let multisampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.multisampled_bind_group_layout_key]),
        )?;
        let singlesampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.singlesampled_bind_group_layout_key]),
        )?;

        let shader_cache_keys = [
            ShaderCacheKey::from(ShaderCacheKeyMaterialClassify {
                msaa_sample_count: Some(4),
            }),
            ShaderCacheKey::from(ShaderCacheKeyMaterialClassify {
                msaa_sample_count: None,
            }),
        ];
        ctx.shaders
            .ensure_keys(ctx.gpu, shader_cache_keys.iter().cloned())
            .await?;

        let multisampled_shader_key = ctx
            .shaders
            .get_key(ctx.gpu, shader_cache_keys[0].clone())
            .await?;
        let singlesampled_shader_key = ctx
            .shaders
            .get_key(ctx.gpu, shader_cache_keys[1].clone())
            .await?;

        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                [
                    ComputePipelineCacheKey::new(
                        multisampled_shader_key,
                        multisampled_pipeline_layout_key,
                    ),
                    ComputePipelineCacheKey::new(
                        singlesampled_shader_key,
                        singlesampled_pipeline_layout_key,
                    ),
                ],
            )
            .await?;

        Ok(Self {
            multisampled_pipeline_key: pipeline_keys[0],
            singlesampled_pipeline_key: pipeline_keys[1],
        })
    }
}
