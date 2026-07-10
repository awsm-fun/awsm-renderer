use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::material_decal::classify::{
    bind_group::DecalClassifyBindGroups, shader::cache_key::ShaderCacheKeyDecalClassify,
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct DecalClassifyPipelines {
    pub cull: ComputePipelineKey,
}

pub struct DecalClassifyPrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
}

impl DecalClassifyPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &DecalClassifyBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                Self::shader_cache_keys(bind_groups, ctx.features.reverse_z),
            )
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

    pub fn shader_cache_keys(
        bind_groups: &DecalClassifyBindGroups,
        reverse_z: bool,
    ) -> Vec<ShaderCacheKey> {
        vec![ShaderCacheKey::from(ShaderCacheKeyDecalClassify {
            hzb_enabled: bind_groups.hzb_enabled,
            reverse_z,
        })]
    }

    pub async fn build_descriptors(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &DecalClassifyBindGroups,
    ) -> Result<DecalClassifyPrewarmDescriptors> {
        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let shader_key = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeyDecalClassify {
                    hzb_enabled: bind_groups.hzb_enabled,
                    reverse_z: ctx.features.reverse_z,
                },
            )
            .await?;
        Ok(DecalClassifyPrewarmDescriptors {
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
