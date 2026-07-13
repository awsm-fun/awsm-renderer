//! SSR pass execution.
//!
//! One compute dispatch reads depth + `normal_tangent` + the resolved
//! single-sample `composite` HDR, marches the reflection ray (linear DDA), and
//! writes reflection-ONLY premultiplied color into the (half-res by default)
//! `ssr` target. [`SsrComposite`] then ADDITIVELY blends that over `composite`
//! with an edge-aware upsample, so bloom + the display pass see reflections in
//! the HDR. Writing to a separate target avoids a read-modify-write hazard, and
//! running post-resolve keeps the color source single-sample under MSAA.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    command::compute_pass::ComputePassDescriptor,
    renderer::AwsmRendererWebGpu,
};

use crate::{
    buffer::mapped_uploader::MappedUploader,
    error::Result,
    render::RenderContext,
    render_passes::{
        ssr::{bind_group::SsrBindGroups, composite::SsrComposite, pipeline::SsrPipelines},
        RenderPassInitContext,
    },
};

/// `SsrParams` — 64-byte uniform (16×f32): the live-tuning knobs (§5a) plus
/// the mirrored reflection-probe box (bytes 32..64 — copied from
/// `Lights::reflection_probe` each frame so the SSR miss fallback projects
/// identically to the material IBL path). Layout must match `struct
/// SsrParams` in `ssr_wgsl/trace.wgsl`.
pub struct SsrParams {
    pub gpu_buffer: web_sys::GpuBuffer,
    raw_data: [u8; Self::BYTE_SIZE],
    uploader: MappedUploader,
    /// Monotonic write counter, packed into the last uniform slot. When
    /// temporal accumulation is on (`temporal_weight > 0`, a RUNTIME gate in
    /// the trace shader) the trace rotates its ray-march jitter by this so the
    /// per-pixel noise VARIES frame to frame and the history accumulation
    /// averages it out (a static jitter pattern would survive any amount of
    /// temporal blending).
    frame: u32,
}

impl SsrParams {
    pub const BYTE_SIZE: usize = 64;

    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("SsrParams"),
                Self::BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;
        let mut params = Self {
            gpu_buffer,
            raw_data: [0; Self::BYTE_SIZE],
            uploader: MappedUploader::new("SsrParams"),
            frame: 0,
        };
        params.pack(1.0, 100.0, 1.0, 96.0, 0.6, 0.1, 0.9, None);
        Ok(params)
    }

    #[allow(clippy::too_many_arguments)]
    fn pack(
        &mut self,
        intensity: f32,
        max_distance: f32,
        thickness: f32,
        max_steps: f32,
        spread_cutoff: f32,
        edge_fade: f32,
        temporal_weight: f32,
        probe: Option<crate::lights::ReflectionProbeBox>,
    ) {
        self.raw_data[0..4].copy_from_slice(&intensity.to_ne_bytes());
        self.raw_data[4..8].copy_from_slice(&max_distance.to_ne_bytes());
        self.raw_data[8..12].copy_from_slice(&thickness.to_ne_bytes());
        self.raw_data[12..16].copy_from_slice(&max_steps.to_ne_bytes());
        self.raw_data[16..20].copy_from_slice(&spread_cutoff.to_ne_bytes());
        self.raw_data[20..24].copy_from_slice(&edge_fade.to_ne_bytes());
        self.raw_data[24..28].copy_from_slice(&temporal_weight.to_ne_bytes());
        // [28..32] = frame counter (as f32) for temporal jitter rotation.
        self.raw_data[28..32].copy_from_slice(&(self.frame as f32).to_ne_bytes());
        // [32..64] = reflection-probe box: center + enabled, half-extents +
        // pad. Zeroed = disabled (same convention as the lights info tail).
        self.raw_data[32..64].fill(0);
        if let Some(probe) = probe {
            for (i, v) in probe.center.iter().enumerate() {
                self.raw_data[32 + i * 4..36 + i * 4].copy_from_slice(&v.to_ne_bytes());
            }
            self.raw_data[44..48].copy_from_slice(&1.0f32.to_ne_bytes());
            for (i, v) in probe.half_extents.iter().enumerate() {
                self.raw_data[48 + i * 4..52 + i * 4].copy_from_slice(&v.to_ne_bytes());
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn write(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        intensity: f32,
        max_distance: f32,
        thickness: f32,
        max_steps: f32,
        spread_cutoff: f32,
        edge_fade: f32,
        temporal_weight: f32,
        probe: Option<crate::lights::ReflectionProbeBox>,
    ) -> Result<()> {
        self.frame = self.frame.wrapping_add(1);
        self.pack(
            intensity,
            max_distance,
            thickness,
            max_steps,
            spread_cutoff,
            edge_fade,
            temporal_weight,
            probe,
        );
        self.uploader.write_dirty_ranges(
            gpu,
            &self.gpu_buffer,
            Self::BYTE_SIZE,
            self.raw_data.as_slice(),
            &[(0, Self::BYTE_SIZE)],
        )?;
        Ok(())
    }
}

pub struct SsrRenderPass {
    pub bind_groups: SsrBindGroups,
    pub pipelines: SsrPipelines,
    pub params: SsrParams,
    pub composite: SsrComposite,
}

impl SsrRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = SsrBindGroups::new(ctx).await?;
        let pipelines = SsrPipelines::new(ctx, &bind_groups).await?;
        let params = SsrParams::new(ctx.gpu)?;
        let composite = SsrComposite::new(ctx).await?;
        Ok(Self {
            bind_groups,
            pipelines,
            params,
            composite,
        })
    }

    /// Trace reflections into the `ssr` target, spatially resolve (edge-aware
    /// 9-tap denoise) into `ssr_resolved`, temporally accumulate (history
    /// reproject + neighborhood clamp) into `ssr_final` when temporal is on,
    /// then additively composite the result over `composite`. `view_width` /
    /// `view_height` are the full-res viewport dims; when `half_res` the `ssr`
    /// target is half-res so all three dispatches cover the halved dimensions
    /// (¼ the rays).
    pub fn render(
        &self,
        ctx: &RenderContext,
        view_width: u32,
        view_height: u32,
        half_res: bool,
    ) -> Result<()> {
        {
            let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
                &ComputePassDescriptor::new(Some("SSR Trace + Resolve + Temporal")).into(),
            ));
            compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.trace)?);
            // Single non-parity trace group — the history ping-pong moved to
            // the temporal pass below.
            compute_pass.set_bind_group(0, self.bind_groups.trace()?, None)?;
            // Trace dims match the `ssr` target: halved when half-res, matching
            // the `((w+1)/2, (h+1)/2)` target sizing in `RenderTexturesInner`.
            let (w, h) = if half_res {
                (view_width.div_ceil(2), view_height.div_ceil(2))
            } else {
                (view_width, view_height)
            };
            let w = w.max(1);
            let h = h.max(1);
            compute_pass.dispatch_workgroups(w.div_ceil(8), Some(h.div_ceil(8)), Some(1));

            // Spatial resolve — the edge-aware denoise between trace and
            // composite: reads the raw trace output + depth, writes the
            // smoothed reflection (rgb AND coverage) into `ssr_resolved` at
            // the same resolution / grid. Same compute-pass scope: WebGPU
            // usage scopes are per-dispatch in compute passes and dispatch
            // ordering makes the trace's storage writes visible here.
            compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.resolve)?);
            compute_pass.set_bind_group(0, self.bind_groups.resolve()?, None)?;
            compute_pass.dispatch_workgroups(w.div_ceil(8), Some(h.div_ceil(8)), Some(1));

            // Temporal accumulation — history reproject + neighborhood clamp
            // over the RESOLVED reflection, writing `ssr_final` (the
            // composite's source) + this frame's history. Frame-parity bind
            // group selects which history texture is read vs written —
            // `ping_pong()` is reachable via the shared `RenderTextures` on
            // the RenderContext, mirroring the effects pass's selector. Same
            // compute-pass scope: dispatch ordering makes the resolve's
            // storage writes visible here.
            if let Some(temporal) = self.pipelines.temporal {
                compute_pass.set_pipeline(ctx.pipelines.compute.get(temporal)?);
                compute_pass.set_bind_group(
                    0,
                    self.bind_groups.temporal(ctx.render_textures.ping_pong())?,
                    None,
                )?;
                compute_pass.dispatch_workgroups(w.div_ceil(8), Some(h.div_ceil(8)), Some(1));
            }
            compute_pass.end();
        }

        // Composite: ADDITIVELY blend the reflection-only accumulated target
        // (`ssr_final` when temporal is on, else `ssr_resolved`) onto
        // `composite` (single-sample resolved HDR) via a fullscreen triangle.
        // Non-reflective pixels wrote 0 so they are untouched; a half-res
        // target edge-aware-upsamples in the shader.
        self.composite.render(ctx)?;
        Ok(())
    }
}
