//! Compute pipeline for the material decal pass.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_decal::{
    bind_group::MaterialDecalBindGroups, shader::cache_key::ShaderCacheKeyMaterialDecal,
};
use crate::render_passes::RenderPassInitContext;

/// Compute pipelines for the decal pass — one per MSAA mode.
/// Both pipelines write to a single-sample `decal_color` (via the
/// shared binding shape); the MSAA path then alpha-blits it onto the
/// multisampled `transparent` via a composite step.
pub struct MaterialDecalPipelines {
    pub singlesampled_pipeline_key: ComputePipelineKey,
    pub multisampled_pipeline_key: ComputePipelineKey,
}

impl MaterialDecalPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialDecalBindGroups,
    ) -> Result<Self> {
        // Pre-warm both shader variants in parallel — the singlesampled
        // and multisampled variants are independent compiles. Without
        // this they serialise through `ctx.shaders.get_key().await`
        // inside each `build()`. Pipeline creation itself stays
        // sequential because `ctx` is `&mut`, but the expensive part
        // (WGSL compile + validation) overlaps.
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                [
                    ShaderCacheKeyMaterialDecal {
                        msaa_sample_count: None,
                        texture_pool_arrays_len: bind_groups.texture_pool_arrays_len,
                        texture_pool_samplers_len: bind_groups.texture_pool_samplers_len,
                    }
                    .into(),
                    ShaderCacheKeyMaterialDecal {
                        msaa_sample_count: Some(4),
                        texture_pool_arrays_len: bind_groups.texture_pool_arrays_len,
                        texture_pool_samplers_len: bind_groups.texture_pool_samplers_len,
                    }
                    .into(),
                ],
            )
            .await?;
        let singlesampled_pipeline_key = build(
            ctx,
            bind_groups,
            bind_groups.main_layout_key_singlesampled,
            None,
        )
        .await?;
        let multisampled_pipeline_key = build(
            ctx,
            bind_groups,
            bind_groups.main_layout_key_multisampled,
            Some(4),
        )
        .await?;
        Ok(Self {
            singlesampled_pipeline_key,
            multisampled_pipeline_key,
        })
    }
}

async fn build(
    ctx: &mut RenderPassInitContext<'_>,
    bind_groups: &MaterialDecalBindGroups,
    main_layout_key: crate::bind_group_layout::BindGroupLayoutKey,
    msaa_sample_count: Option<u32>,
) -> Result<ComputePipelineKey> {
    let pipeline_layout_cache_key =
        PipelineLayoutCacheKey::new(vec![main_layout_key, bind_groups.texture_pool_layout_key]);
    let pipeline_layout_key =
        ctx.pipeline_layouts
            .get_key(ctx.gpu, ctx.bind_group_layouts, pipeline_layout_cache_key)?;

    let shader_key = ctx
        .shaders
        .get_key(
            ctx.gpu,
            ShaderCacheKeyMaterialDecal {
                msaa_sample_count,
                texture_pool_arrays_len: bind_groups.texture_pool_arrays_len,
                texture_pool_samplers_len: bind_groups.texture_pool_samplers_len,
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
