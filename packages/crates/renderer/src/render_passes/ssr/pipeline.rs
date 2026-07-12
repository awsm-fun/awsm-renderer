//! SSR pass compute pipelines.
//!
//! Self-contained (like bloom): compiles its own shaders + pipelines rather
//! than joining the cross-renderer pool. Two stages: the trace (always the
//! Hi-Z traversal when the HZB exists, else linear-DDA; glossy / temporal / half-res
//! axes each key a distinct cache key → distinct compiled shader, §5a) and the
//! spatial resolve (the 9-tap edge-aware denoise between trace and composite —
//! its only axes are the MSAA depth-binding type + reverse-Z).

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::ssr::{
    bind_group::SsrBindGroups,
    shader::cache_key::{ShaderCacheKeySsrResolve, ShaderCacheKeySsrTrace, SsrMode, SsrTrace},
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct SsrPipelines {
    pub trace: ComputePipelineKey,
    /// Spatial resolve — dispatched right after the trace, same grid.
    pub resolve: ComputePipelineKey,
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
        hzb: bool,
    ) -> ShaderCacheKeySsrTrace {
        ShaderCacheKeySsrTrace {
            mode: SsrMode::Glossy,
            // Hi-Z when the pyramid exists (gpu_culling capability — a
            // static, per-session fact, so this never flips per frame);
            // per-pixel linear DDA otherwise.
            trace: if hzb {
                SsrTrace::HiZ
            } else {
                SsrTrace::LinearDda
            },
            temporal,
            half_res,
            multisampled_geometry: multisampled,
            reverse_z,
        }
    }

    /// The spatial-resolve variant: runs at the SSR target's own resolution
    /// (whatever that is — it reads its output dims at runtime), so only the
    /// depth-binding type (MSAA) + depth convention are axes.
    fn resolve_key(multisampled: bool, reverse_z: bool) -> ShaderCacheKeySsrResolve {
        ShaderCacheKeySsrResolve {
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
                    ctx.features.gpu_culling,
                ),
            )
            .await?;

        let trace_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.layout_key]),
        )?;
        let resolve_layout = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            PipelineLayoutCacheKey::new(vec![bind_groups.resolve_layout_key]),
        )?;

        let trace_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                Self::m1_key(
                    multisampled,
                    half_res,
                    temporal,
                    ctx.features.reverse_z,
                    ctx.features.gpu_culling,
                ),
            )
            .await?;
        let resolve_shader = ctx
            .shaders
            .get_key(
                ctx.gpu,
                Self::resolve_key(multisampled, ctx.features.reverse_z),
            )
            .await?;

        let cache_keys = vec![
            ComputePipelineCacheKey::new(trace_shader, trace_layout),
            ComputePipelineCacheKey::new(resolve_shader, resolve_layout),
        ];

        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;

        Ok(Self {
            trace: pipeline_keys[0],
            resolve: pipeline_keys[1],
        })
    }

    pub fn shader_cache_keys(
        anti_aliasing: &crate::anti_alias::AntiAliasing,
        half_res: bool,
        temporal: bool,
        reverse_z: bool,
        hzb: bool,
    ) -> Vec<ShaderCacheKey> {
        let multisampled = anti_aliasing.msaa_sample_count.is_some();
        vec![
            ShaderCacheKey::from(Self::m1_key(
                multisampled,
                half_res,
                temporal,
                reverse_z,
                hzb,
            )),
            ShaderCacheKey::from(Self::resolve_key(multisampled, reverse_z)),
        ]
    }
}
