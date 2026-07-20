//! SMAA compute pipelines (edges + weights). Self-contained ensure, mirroring
//! the bloom pass (two tiny static compute shaders — no cross-renderer pool
//! membership needed).

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::smaa::{
    bind_group::SmaaBindGroups,
    shader::cache_key::{ShaderCacheKeySmaa, SmaaStep},
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct SmaaPipelines {
    pub edges: ComputePipelineKey,
    pub weights: ComputePipelineKey,
}

impl SmaaPipelines {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &SmaaBindGroups,
    ) -> Result<Self> {
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                vec![
                    ShaderCacheKey::from(ShaderCacheKeySmaa {
                        step: SmaaStep::Edges,
                    }),
                    ShaderCacheKey::from(ShaderCacheKeySmaa {
                        step: SmaaStep::Weights,
                    }),
                ],
            )
            .await?;

        let edges_pipeline_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.edges_layout_key]),
        )?;
        let weights_pipeline_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.weights_layout_key]),
        )?;

        let edges_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeySmaa {
                    step: SmaaStep::Edges,
                },
            )
            .await?;
        let weights_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                ShaderCacheKeySmaa {
                    step: SmaaStep::Weights,
                },
            )
            .await?;

        let keys = ctx
            .pipelines
            .compute
            .ensure_keys(
                ctx.gpu,
                ctx.shaders,
                ctx.pipeline_layouts,
                vec![
                    ComputePipelineCacheKey::new(edges_shader, edges_pipeline_layout),
                    ComputePipelineCacheKey::new(weights_shader, weights_pipeline_layout),
                ],
            )
            .await?;

        Ok(Self {
            edges: keys[0],
            weights: keys[1],
        })
    }
}
