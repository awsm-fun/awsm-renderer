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

/// `SsrParams` — 32-byte uniform (8×f32): the live-tuning knobs (§5a). Layout
/// must match `struct SsrParams` in `ssr_wgsl/trace.wgsl`.
pub struct SsrParams {
    pub gpu_buffer: web_sys::GpuBuffer,
    raw_data: [u8; Self::BYTE_SIZE],
    uploader: MappedUploader,
}

impl SsrParams {
    pub const BYTE_SIZE: usize = 32;

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
        };
        params.pack(1.0, 100.0, 1.0, 96.0, 0.6, 0.1, 0.9);
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
    ) {
        self.raw_data[0..4].copy_from_slice(&intensity.to_ne_bytes());
        self.raw_data[4..8].copy_from_slice(&max_distance.to_ne_bytes());
        self.raw_data[8..12].copy_from_slice(&thickness.to_ne_bytes());
        self.raw_data[12..16].copy_from_slice(&max_steps.to_ne_bytes());
        self.raw_data[16..20].copy_from_slice(&spread_cutoff.to_ne_bytes());
        self.raw_data[20..24].copy_from_slice(&edge_fade.to_ne_bytes());
        self.raw_data[24..28].copy_from_slice(&temporal_weight.to_ne_bytes());
        // [28..32] = padding, left zero.
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
    ) -> Result<()> {
        self.pack(
            intensity,
            max_distance,
            thickness,
            max_steps,
            spread_cutoff,
            edge_fade,
            temporal_weight,
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

    /// Trace reflections into the `ssr` target, then additively composite that
    /// over `composite`. `view_width` / `view_height` are the full-res viewport
    /// dims; when `half_res` the `ssr` target is half-res so the trace dispatches
    /// over the halved dimensions (¼ the rays).
    pub fn render(
        &self,
        ctx: &RenderContext,
        view_width: u32,
        view_height: u32,
        half_res: bool,
    ) -> Result<()> {
        {
            let compute_pass = ctx
                .command_encoder
                .begin_compute_pass(Some(&ComputePassDescriptor::new(Some("SSR Trace")).into()));
            compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.trace)?);
            // M3 temporal: select the frame-parity bind group (write current
            // history / read previous). `ping_pong()` is reachable via the
            // shared `RenderTextures` on the RenderContext — mirrors how the
            // effects pass threads its ping-pong selector. Non-temporal ignores
            // the arg (single slot-0 group).
            compute_pass.set_bind_group(
                0,
                self.bind_groups.trace(ctx.render_textures.ping_pong())?,
                None,
            )?;
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
            compute_pass.end();
        }

        // Composite: ADDITIVELY blend the reflection-only `ssr` target onto
        // `composite` (single-sample resolved HDR) via a fullscreen triangle +
        // linear sampler. Non-reflective pixels wrote 0 so they are untouched;
        // a half-res `ssr` target bilinearly upsamples for free.
        self.composite.render(ctx)?;
        Ok(())
    }
}
