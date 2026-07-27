//! Volumetrics compute pipelines.
//!
//! Two, sharing one pipeline layout (both stages bind the same three groups —
//! the inject/integrate difference is which volume sits in slots 2 and 3, not
//! the layout). Self-contained like bloom: `new` ensures its own shader +
//! pipeline cache keys rather than joining the cross-renderer pool.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::volumetrics::{
    bind_group::VolumetricsBindGroups,
    shader::cache_key::{ShaderCacheKeyVolumetrics, VolumetricsStage},
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct VolumetricsPipelines {
    pub inject: ComputePipelineKey,
    pub integrate: ComputePipelineKey,
}

impl VolumetricsPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &VolumetricsBindGroups,
        reverse_z: bool,
        temporal: bool,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(ctx.gpu, Self::shader_cache_keys(reverse_z, temporal))
            .await?;

        let pipeline_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![
                bind_groups.volume_layout_key,
                bind_groups.lights_layout_key,
                bind_groups.shadows_layout_key,
            ]),
        )?;

        let mut cache_keys = Vec::with_capacity(2);
        for stage in [VolumetricsStage::Inject, VolumetricsStage::Integrate] {
            let shader = ctx
                .shaders
                .get_key(
                    ctx.gpu,
                    ShaderCacheKeyVolumetrics {
                        stage,
                        reverse_z,
                        temporal,
                    },
                )
                .await?;
            cache_keys.push(ComputePipelineCacheKey::new(shader, pipeline_layout));
        }

        let keys = ctx
            .pipelines
            .compute
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;

        Ok(Self {
            inject: keys[0],
            integrate: keys[1],
        })
    }

    pub fn shader_cache_keys(reverse_z: bool, temporal: bool) -> Vec<ShaderCacheKey> {
        vec![
            ShaderCacheKey::from(ShaderCacheKeyVolumetrics {
                stage: VolumetricsStage::Inject,
                reverse_z,
                temporal,
            }),
            ShaderCacheKey::from(ShaderCacheKeyVolumetrics {
                stage: VolumetricsStage::Integrate,
                reverse_z,
                temporal,
            }),
        ]
    }
}
