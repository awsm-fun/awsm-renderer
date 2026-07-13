//! SSR pass compute pipelines.
//!
//! Self-contained (like bloom): compiles its own shaders + pipelines rather
//! than joining the cross-renderer pool. Three stages: the trace (always the
//! Hi-Z traversal when the HZB exists, else linear-DDA; glossy / half-res axes
//! each key a distinct cache key → distinct compiled shader, §5a), the spatial
//! resolve (the 9-tap edge-aware denoise between trace and composite — its
//! only axes are the MSAA depth-binding type + reverse-Z), and the temporal
//! accumulation (history reproject + neighborhood clamp after the resolve —
//! same axes as the resolve, compiled ONLY when `ssr.temporal`; the toggle
//! reconstructs the whole SSR pass, same as `resolution_scale`).

use crate::error::Result;
use crate::pipeline_layouts::PipelineLayoutCacheKey;
use crate::pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey};
use crate::render_passes::ssr::{
    bind_group::SsrBindGroups,
    shader::cache_key::{
        ShaderCacheKeySsrResolve, ShaderCacheKeySsrTemporal, ShaderCacheKeySsrTrace, SsrMode,
        SsrTrace,
    },
};
use crate::render_passes::RenderPassInitContext;
use crate::shaders::ShaderCacheKey;

pub struct SsrPipelines {
    pub trace: ComputePipelineKey,
    /// Spatial resolve — dispatched right after the trace, same grid.
    pub resolve: ComputePipelineKey,
    /// Temporal accumulation — dispatched right after the resolve, same grid.
    /// `None` unless `post_processing.ssr.temporal` at construction (the
    /// toggle reconstructs the SSR pass, so this never flips in place).
    pub temporal: Option<ComputePipelineKey>,
    /// Software-BVH trace — dispatched BEFORE the screen-space trace.
    /// `None` unless `post_processing.ssr.bvh_reflections` at construction
    /// (same reconstruct-on-toggle semantics as `temporal`).
    pub bvh_trace: Option<ComputePipelineKey>,
}

impl SsrPipelines {
    /// The M2b variant: glossy reflection (spread-driven blur — spread 0 is a
    /// sharp mirror via a runtime branch), linear DDA. `half_res`
    /// (resolution_scale < 1.0) selects the half-res trace variant; the shader
    /// body is identical (it reads its output dims at runtime), but the axis
    /// still keys a distinct compiled pipeline so a resolution_scale change
    /// recompiles. `multisampled` matches the current AA (MSAA → multisampled
    /// depth/normal). Temporal is NOT an axis here anymore — the trace's
    /// per-frame jitter rotation is a runtime uniform gate.
    fn m1_key(
        multisampled: bool,
        half_res: bool,
        reverse_z: bool,
        hzb: bool,
        debug: u32,
        bvh: bool,
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
            half_res,
            multisampled_geometry: multisampled,
            reverse_z,
            debug,
            bvh,
        }
    }

    /// The software-BVH trace variant (docs/plans/bvh-reflections.md).
    fn bvh_key(
        multisampled: bool,
        reverse_z: bool,
    ) -> super::shader::cache_key::ShaderCacheKeySsrBvhTrace {
        super::shader::cache_key::ShaderCacheKeySsrBvhTrace {
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

    /// The temporal-accumulation variant: same axes as the resolve (it also
    /// runs at the SSR target's own resolution and reads the full-res depth).
    fn temporal_key(multisampled: bool, reverse_z: bool) -> ShaderCacheKeySsrTemporal {
        ShaderCacheKeySsrTemporal {
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
        let debug = ctx.post_processing.ssr.debug;
        let bvh = ctx.post_processing.ssr.bvh_reflections;
        ctx.shaders
            .ensure_keys(
                ctx.gpu,
                Self::shader_cache_keys(
                    ctx.anti_aliasing,
                    half_res,
                    temporal,
                    ctx.features.reverse_z,
                    ctx.features.gpu_culling,
                    debug,
                    bvh,
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
                    ctx.features.reverse_z,
                    ctx.features.gpu_culling,
                    debug,
                    bvh,
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

        let mut cache_keys = vec![
            ComputePipelineCacheKey::new(trace_shader, trace_layout),
            ComputePipelineCacheKey::new(resolve_shader, resolve_layout),
        ];

        // Temporal pipeline — same lazy semantics as the old temporal trace
        // variant: only built when the temporal axis is on (the toggle
        // reconstructs the whole SSR pass via `set_post_processing`).
        if temporal {
            let temporal_layout_key = bind_groups
                .temporal_layout_key
                .expect("SSR temporal layout missing despite ssr.temporal on");
            let temporal_layout = ctx.pipeline_layouts.get_key(
                ctx.gpu,
                ctx.bind_group_layouts,
                PipelineLayoutCacheKey::new(vec![temporal_layout_key]),
            )?;
            let temporal_shader = ctx
                .shaders
                .get_key(
                    ctx.gpu,
                    Self::temporal_key(multisampled, ctx.features.reverse_z),
                )
                .await?;
            cache_keys.push(ComputePipelineCacheKey::new(
                temporal_shader,
                temporal_layout,
            ));
        }

        // Software-BVH pipeline — lazy exactly like temporal.
        if bvh {
            let bvh_layout_key = bind_groups
                .bvh_layout_key
                .expect("SSR bvh layout missing despite ssr.bvh_reflections on");
            let bvh_layout = ctx.pipeline_layouts.get_key(
                ctx.gpu,
                ctx.bind_group_layouts,
                PipelineLayoutCacheKey::new(vec![bvh_layout_key]),
            )?;
            let bvh_shader = ctx
                .shaders
                .get_key(ctx.gpu, Self::bvh_key(multisampled, ctx.features.reverse_z))
                .await?;
            cache_keys.push(ComputePipelineCacheKey::new(bvh_shader, bvh_layout));
        }

        let pipeline_keys = ctx
            .pipelines
            .compute
            .ensure_keys(ctx.gpu, ctx.shaders, ctx.pipeline_layouts, cache_keys)
            .await?;

        let mut next = 2usize;
        let temporal_key_slot = temporal.then(|| {
            let k = pipeline_keys[next];
            next += 1;
            k
        });
        let bvh_key_slot = bvh.then(|| pipeline_keys[next]);
        Ok(Self {
            trace: pipeline_keys[0],
            resolve: pipeline_keys[1],
            temporal: temporal_key_slot,
            bvh_trace: bvh_key_slot,
        })
    }

    pub fn shader_cache_keys(
        anti_aliasing: &crate::anti_alias::AntiAliasing,
        half_res: bool,
        temporal: bool,
        reverse_z: bool,
        hzb: bool,
        debug: u32,
        bvh: bool,
    ) -> Vec<ShaderCacheKey> {
        let multisampled = anti_aliasing.msaa_sample_count.is_some();
        let mut keys = vec![
            ShaderCacheKey::from(Self::m1_key(
                multisampled,
                half_res,
                reverse_z,
                hzb,
                debug,
                bvh,
            )),
            ShaderCacheKey::from(Self::resolve_key(multisampled, reverse_z)),
        ];
        if temporal {
            keys.push(ShaderCacheKey::from(Self::temporal_key(
                multisampled,
                reverse_z,
            )));
        }
        if bvh {
            keys.push(ShaderCacheKey::from(Self::bvh_key(multisampled, reverse_z)));
        }
        keys
    }
}
