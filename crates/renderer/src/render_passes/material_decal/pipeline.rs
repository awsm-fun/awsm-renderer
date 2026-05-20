//! Compute pipeline for the material decal pass.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_decal::{
    bind_group::MaterialDecalBindGroups, shader::cache_key::ShaderCacheKeyMaterialDecal,
};
use crate::render_passes::RenderPassInitContext;

/// Compute pipelines for the decal pass — one per MSAA mode.
/// The MSAA pipeline is unused in v1 (the transparent texture isn't
/// storage-bindable when multisampled, so the render pass skips the
/// dispatch); the key is held so a future "ping-pong via dedicated
/// decal_color_tex" follow-up can land without rebuilding the cache.
pub struct MaterialDecalPipelines {
    pub singlesampled_pipeline_key: ComputePipelineKey,
}

impl MaterialDecalPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialDecalBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_cache_key = PipelineLayoutCacheKey::new(vec![
            bind_groups.main_layout_key_singlesampled,
            bind_groups.texture_pool_layout_key,
        ]);
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            pipeline_layout_cache_key,
        )?;

        let shader_key = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyMaterialDecal {
                    msaa_sample_count: None,
                    texture_pool_arrays_len: bind_groups.texture_pool_arrays_len,
                    texture_pool_samplers_len: bind_groups.texture_pool_samplers_len,
                },
            )
            .await?;

        let singlesampled_pipeline_key = ctx
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
            singlesampled_pipeline_key,
        })
    }
}
