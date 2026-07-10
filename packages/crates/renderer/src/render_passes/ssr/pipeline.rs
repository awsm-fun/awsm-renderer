//! SSR pass compute pipeline.
//!
//! Self-contained (like bloom): compiles its own shader + pipeline rather than
//! joining the cross-renderer pool. M1 builds the single mirror / linear-DDA /
//! non-temporal / full-res variant; M2–M3 add the glossy / Hi-Z / temporal /
//! half-res variants (each a distinct cache key → distinct compiled shader,
//! §5a).

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::ssr::{
    bind_group::SsrBindGroups,
    shader::cache_key::{ShaderCacheKeySsr, SsrMode, SsrTrace},
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct SsrPipelines {
    pub trace: ComputePipelineKey,
}

impl SsrPipelines {
    /// The M2b variant: glossy reflection (spread-driven blur — spread 0 is a
    /// sharp mirror via a runtime branch), linear DDA, no temporal. `half_res`
    /// (resolution_scale < 1.0) selects the half-res trace variant; the shader
    /// body is identical (it reads its output dims at runtime), but the axis
    /// still keys a distinct compiled pipeline so a resolution_scale change
    /// recompiles. `multisampled` matches the current AA (MSAA → multisampled
    /// depth/normal).
    fn m1_key(
        multisampled: bool,
        half_res: bool,
        temporal: bool,
        reverse_z: bool,
    ) -> ShaderCacheKeySsr {
        ShaderCacheKeySsr {
            mode: SsrMode::Glossy,
            // M2c: production marches the min-Z pyramid (Hi-Z). `SsrTrace::PRODUCTION`
            // is the shared source of truth with `SsrBindGroups` so the compiled
            // shader's pyramid binding always matches the bind-group layout.
            trace: SsrTrace::PRODUCTION,
            temporal,
            half_res,
            multisampled_geometry: multisampled,
            reverse_z,
        }
    }

    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &SsrBindGroups,
    ) -> Result<Self> {
        let multisampled = ctx.anti_aliasing.msaa_sample_count.is_some();
        let half_res = ctx.post_processing.ssr.resolution_scale < 1.0;
        let temporal = ctx.post_processing.ssr.temporal;
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                Self::shader_cache_keys(
                    ctx.anti_aliasing,
                    half_res,
                    temporal,
                    ctx.features.reverse_z,
                ),
            )
            .await?;

        let pipeline_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;

        let trace_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                Self::m1_key(multisampled, half_res, temporal, ctx.features.reverse_z),
            )
            .await?;

        let cache_keys = vec![ComputePipelineCacheKey::new(trace_shader, pipeline_layout)];

        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;

        Ok(Self {
            trace: pipeline_keys[0],
        })
    }

    pub fn shader_cache_keys(
        anti_aliasing: &crate::anti_alias::AntiAliasing,
        half_res: bool,
        temporal: bool,
        reverse_z: bool,
    ) -> Vec<ShaderCacheKey> {
        let multisampled = anti_aliasing.msaa_sample_count.is_some();
        vec![ShaderCacheKey::from(Self::m1_key(
            multisampled,
            half_res,
            temporal,
            reverse_z,
        ))]
    }
}
