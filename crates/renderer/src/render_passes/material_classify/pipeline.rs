//! Compute pipeline for the material classify pass.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_classify::{
    bind_group::MaterialClassifyBindGroups, shader::cache_key::ShaderCacheKeyMaterialClassify,
};
use crate::render_passes::RenderPassInitContext;

/// One compute pipeline per MSAA mode. The classify shader only
/// touches sample 0 of the visibility texture, but WGSL still has
/// distinct `texture_multisampled_2d<u32>` / `texture_2d<u32>` types
/// so the pipeline layout has to match.
pub struct MaterialClassifyPipelines {
    pub multisampled_pipeline_key: ComputePipelineKey,
    pub singlesampled_pipeline_key: ComputePipelineKey,
}

impl MaterialClassifyPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialClassifyBindGroups,
    ) -> Result<Self> {
        // Pre-warm both MSAA variants concurrently. Same pattern as
        // material_opaque/decal: compile in parallel, build pipelines
        // serially (since `ctx` is `&mut`).
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                [
                    ShaderCacheKeyMaterialClassify {
                        msaa_sample_count: Some(4),
                    }
                    .into(),
                    ShaderCacheKeyMaterialClassify {
                        msaa_sample_count: None,
                    }
                    .into(),
                ],
            )
            .await?;
        let multisampled = build(ctx, bind_groups, true).await?;
        let singlesampled = build(ctx, bind_groups, false).await?;
        Ok(Self {
            multisampled_pipeline_key: multisampled,
            singlesampled_pipeline_key: singlesampled,
        })
    }
}

async fn build(
    ctx: &mut RenderPassInitContext<'_>,
    bind_groups: &MaterialClassifyBindGroups,
    multisampled_geometry: bool,
) -> Result<ComputePipelineKey> {
    let layout_key = if multisampled_geometry {
        bind_groups.multisampled_bind_group_layout_key
    } else {
        bind_groups.singlesampled_bind_group_layout_key
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
            ShaderCacheKeyMaterialClassify {
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
