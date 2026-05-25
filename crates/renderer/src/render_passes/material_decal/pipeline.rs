//! Compute pipeline for the material decal pass.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_decal::{
    bind_group::MaterialDecalBindGroups, shader::cache_key::ShaderCacheKeyMaterialDecal,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

/// Compute pipelines for the decal pass — one per MSAA mode.
/// Both pipelines write to a single-sample `decal_color` (via the
/// shared binding shape); the MSAA path then alpha-blits it onto the
/// frame's `transparent` via a composite step.
pub struct MaterialDecalPipelines {
    pub singlesampled_pipeline_key: ComputePipelineKey,
    pub multisampled_pipeline_key: ComputePipelineKey,
}

impl MaterialDecalPipelines {
    /// Builds both MSAA variants concurrently via batched
    /// `Shaders::ensure_keys` + `ComputePipelines::ensure_keys`.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialDecalBindGroups,
    ) -> Result<Self> {
        let singlesampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                bind_groups.main_layout_key_singlesampled,
                bind_groups.texture_pool_layout_key,
            ]),
        )?;
        let multisampled_pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                bind_groups.main_layout_key_multisampled,
                bind_groups.texture_pool_layout_key,
            ]),
        )?;

        let shader_cache_keys = [
            ShaderCacheKey::from(ShaderCacheKeyMaterialDecal {
                msaa_sample_count: None,
                texture_pool_arrays_len: bind_groups.texture_pool_arrays_len,
                texture_pool_samplers_len: bind_groups.texture_pool_samplers_len,
            }),
            ShaderCacheKey::from(ShaderCacheKeyMaterialDecal {
                msaa_sample_count: Some(4),
                texture_pool_arrays_len: bind_groups.texture_pool_arrays_len,
                texture_pool_samplers_len: bind_groups.texture_pool_samplers_len,
            }),
        ];
        ctx.shaders
            .ensure_keys(ctx.gpu, shader_cache_keys.iter().cloned())
            .await?;

        let singlesampled_shader_key = ctx
            .shaders
            .get_key(ctx.gpu, shader_cache_keys[0].clone())
            .await?;
        let multisampled_shader_key = ctx
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
                        singlesampled_shader_key,
                        singlesampled_pipeline_layout_key,
                    ),
                    ComputePipelineCacheKey::new(
                        multisampled_shader_key,
                        multisampled_pipeline_layout_key,
                    ),
                ],
            )
            .await?;

        Ok(Self {
            singlesampled_pipeline_key: pipeline_keys[0],
            multisampled_pipeline_key: pipeline_keys[1],
        })
    }
}
