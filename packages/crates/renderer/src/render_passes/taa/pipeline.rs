//! TAA resolve compute pipeline. Self-contained like SMAA / SSR: compiles its
//! own shader + pipeline when the pass is built (TAA turned on), nothing when
//! TAA is off.

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::taa::{bind_group::TaaBindGroups, shader::cache_key::ShaderCacheKeyTaa};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct TaaPipelines {
    pub resolve: ComputePipelineKey,
}

impl TaaPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &TaaBindGroups,
        reactive: bool,
    ) -> Result<Self> {
        let key = Self::shader_cache_key(ctx.features.reverse_z, reactive);
        ctx.shaders
            .ensure_keys(ctx.gpu, vec![ShaderCacheKey::from(key.clone())])
            .await?;
        let layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let shader = ctx.shaders.get_key(ctx.gpu, key).await?;
        let keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                vec![ComputePipelineCacheKey::new(shader, layout)],
            )
            .await?;
        Ok(Self { resolve: keys[0] })
    }

    /// Every pipeline the pass holds.
    pub(crate) fn keys(&self) -> Vec<ComputePipelineKey> {
        vec![self.resolve]
    }

    pub fn shader_cache_key(reverse_z: bool, reactive: bool) -> ShaderCacheKeyTaa {
        ShaderCacheKeyTaa {
            reverse_z,
            reactive,
        }
    }
}
