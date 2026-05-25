//! Compute pipeline for the material classify pass.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_classify::{
    bind_group::MaterialClassifyBindGroups, shader::cache_key::ShaderCacheKeyMaterialClassify,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct MaterialClassifyPipelines {
    pub multisampled_pipeline_key: ComputePipelineKey,
    pub singlesampled_pipeline_key: ComputePipelineKey,
}

/// Slot order: [0] multisampled, [1] singlesampled.
pub struct MaterialClassifyPrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
}

impl MaterialClassifyPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialClassifyBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys())
            .await?;
        let descs = Self::build_descriptors(ctx, bind_groups).await?;
        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                descs.pipeline_cache_keys.clone(),
            )
            .await?;
        Ok(Self::from_resolved(pipeline_keys))
    }

    pub fn shader_cache_keys() -> Vec<ShaderCacheKey> {
        vec![
            ShaderCacheKey::from(ShaderCacheKeyMaterialClassify {
                msaa_sample_count: Some(4),
            }),
            ShaderCacheKey::from(ShaderCacheKeyMaterialClassify {
                msaa_sample_count: None,
            }),
        ]
    }

    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialClassifyBindGroups,
    ) -> Result<MaterialClassifyPrewarmDescriptors> {
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

        let multisampled_shader_key = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyMaterialClassify {
                    msaa_sample_count: Some(4),
                },
            )
            .await?;
        let singlesampled_shader_key = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyMaterialClassify {
                    msaa_sample_count: None,
                },
            )
            .await?;

        Ok(MaterialClassifyPrewarmDescriptors {
            pipeline_cache_keys: vec![
                ComputePipelineCacheKey::new(
                    multisampled_shader_key,
                    multisampled_pipeline_layout_key,
                ),
                ComputePipelineCacheKey::new(
                    singlesampled_shader_key,
                    singlesampled_pipeline_layout_key,
                ),
            ],
        })
    }

    pub fn from_resolved(pipeline_keys: Vec<ComputePipelineKey>) -> Self {
        Self {
            multisampled_pipeline_key: pipeline_keys[0],
            singlesampled_pipeline_key: pipeline_keys[1],
        }
    }
}
