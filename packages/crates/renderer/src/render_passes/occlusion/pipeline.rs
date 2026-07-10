//! Occlusion-cull compute pipeline.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::occlusion::{
    bind_group::OcclusionBindGroups, shader::cache_key::ShaderCacheKeyOcclusionCull,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct OcclusionPipelines {
    pub cull: ComputePipelineKey,
}

pub struct OcclusionPrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
}

impl OcclusionPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &OcclusionBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys(ctx.features.reverse_z))
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

    pub fn shader_cache_keys(reverse_z: bool) -> Vec<ShaderCacheKey> {
        vec![ShaderCacheKey::from(ShaderCacheKeyOcclusionCull {
            reverse_z,
        })]
    }

    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &OcclusionBindGroups,
    ) -> Result<OcclusionPrewarmDescriptors> {
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let shader_key = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyOcclusionCull {
                    reverse_z: ctx.features.reverse_z,
                },
            )
            .await?;
        Ok(OcclusionPrewarmDescriptors {
            pipeline_cache_keys: vec![ComputePipelineCacheKey::new(
                shader_key,
                pipeline_layout_key,
            )],
        })
    }

    pub fn from_resolved(pipeline_keys: Vec<ComputePipelineKey>) -> Self {
        Self {
            cull: pipeline_keys[0],
        }
    }
}
