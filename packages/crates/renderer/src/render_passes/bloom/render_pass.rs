//! Bloom build render pass execution.
//!
//! One compute pass, three step kinds (COD/Jimenez mip-pyramid bloom):
//! 1. **Prefilter** — composite (full-res) → pyramid mip 0, half-res, with a
//!    soft-knee threshold. Dispatched at mip-0 dims / 8.
//! 2. **Downsample** — pyramid mip N-1 → mip N for `N = 1..mip_count`, each a
//!    plain 13-tap Jimenez step. Dispatched at each destination mip dims / 8.
//! 3. **Combine** — mip-sum upsample of the whole pyramid into the full-res
//!    `bloom` target (this IS the wide glow). Dispatched at viewport dims / 8.
//!
//! All three coalesce into a single `begin_compute_pass`, mirroring the HZB
//! build; WebGPU inserts the storage-write→sample barriers between dispatches.

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
        bloom::{bind_group::BloomBindGroups, pipeline::BloomPipelines, texture::BloomTexture},
        RenderPassInitContext,
    },
};

/// `BloomParams` — 16-byte uniform: `threshold`, `knee`, `intensity`,
/// `scatter` (4×f32). Seeded with sane defaults so bloom renders before any
/// config is wired.
pub struct BloomParams {
    pub gpu_buffer: web_sys::GpuBuffer,
    raw_data: [u8; Self::BYTE_SIZE],
    uploader: MappedUploader,
}

impl BloomParams {
    pub const BYTE_SIZE: usize = 16;

    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("BloomParams"),
                Self::BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        let mut params = Self {
            gpu_buffer,
            raw_data: [0; Self::BYTE_SIZE],
            uploader: MappedUploader::new("BloomParams"),
        };
        // Seed with defaults so the pass renders sanely before config is wired.
        params.pack(1.0, 0.5, 1.0, 1.0);
        Ok(params)
    }

    fn pack(&mut self, threshold: f32, knee: f32, intensity: f32, scatter: f32) {
        self.raw_data[0..4].copy_from_slice(&threshold.to_ne_bytes());
        self.raw_data[4..8].copy_from_slice(&knee.to_ne_bytes());
        self.raw_data[8..12].copy_from_slice(&intensity.to_ne_bytes());
        self.raw_data[12..16].copy_from_slice(&scatter.to_ne_bytes());
    }

    /// Packs + uploads the params via the mapped-ring path.
    pub fn write(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        threshold: f32,
        knee: f32,
        intensity: f32,
        scatter: f32,
    ) -> Result<()> {
        self.pack(threshold, knee, intensity, scatter);
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

pub struct BloomRenderPass {
    pub bind_groups: BloomBindGroups,
    pub pipelines: BloomPipelines,
    /// The bloom pyramid texture. Owned by the pass so resize logic stays
    /// local; `bind_groups.recreate` rebuilds against this.
    pub texture: BloomTexture,
    /// Live `BloomParams` uniform (threshold / knee / intensity / scatter).
    pub params: BloomParams,
}

impl BloomRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = BloomBindGroups::new(ctx).await?;
        let pipelines = BloomPipelines::new(ctx, &bind_groups).await?;
        // Tiny initial allocation; the per-frame resize hook recreates against
        // the live viewport before the first dispatch.
        let texture = BloomTexture::new(ctx.gpu, 1, 1)?;
        let params = BloomParams::new(ctx.gpu)?;
        Ok(Self {
            bind_groups,
            pipelines,
            texture,
            params,
        })
    }

    /// Re-allocates the bloom pyramid to match the current viewport. Returns
    /// `true` when a new texture was created — the caller marks the dependent
    /// bind groups dirty in that case.
    pub fn ensure_size(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        view_width: u32,
        view_height: u32,
    ) -> Result<bool> {
        // BloomTexture stores mip 0 at HALF the viewport; compare against the
        // viewport it was built from (2× the pyramid base).
        let cur_view_w = self.texture.base_width * 2;
        let cur_view_h = self.texture.base_height * 2;
        if cur_view_w == view_width.max(1) && cur_view_h == view_height.max(1) {
            return Ok(false);
        }
        self.texture = BloomTexture::new(gpu, view_width, view_height)?;
        Ok(true)
    }

    /// Builds the bloom pyramid + wide glow for the current frame:
    /// 1. Prefilter composite → pyramid mip 0.
    /// 2. Downsample mip 0 → 1, 1 → 2, …, mip_count-2 → mip_count-1.
    /// 3. Combine the whole pyramid into the full-res bloom target.
    ///
    /// `view_width` / `view_height` size the final combine dispatch (full-res).
    pub fn render(&self, ctx: &RenderContext, view_width: u32, view_height: u32) -> Result<()> {
        let compute_pass = ctx
            .command_encoder
            .begin_compute_pass(Some(&ComputePassDescriptor::new(Some("Bloom Build")).into()));

        // Prefilter — composite → pyramid mip 0 (half-res).
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.prefilter)?);
        compute_pass.set_bind_group(0, self.bind_groups.prefilter()?, None)?;
        let (mip0_w, mip0_h) = self.texture.mip_dims(0);
        compute_pass.dispatch_workgroups(mip0_w.div_ceil(8), Some(mip0_h.div_ceil(8)), Some(1));

        // Downsample — mip 0→1, 1→2, …, N-2→N-1, all in the same pass.
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.downsample)?);
        for transition in 0..(self.texture.mip_count.saturating_sub(1)) as usize {
            compute_pass.set_bind_group(0, self.bind_groups.downsample_at(transition)?, None)?;
            let (dst_w, dst_h) = self.texture.mip_dims((transition + 1) as u32);
            compute_pass.dispatch_workgroups(dst_w.div_ceil(8), Some(dst_h.div_ceil(8)), Some(1));
        }

        // Combine — mip-sum upsample into the full-res bloom target.
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.combine)?);
        compute_pass.set_bind_group(0, self.bind_groups.combine()?, None)?;
        let full_w = view_width.max(1);
        let full_h = view_height.max(1);
        compute_pass.dispatch_workgroups(full_w.div_ceil(8), Some(full_h.div_ceil(8)), Some(1));

        compute_pass.end();
        Ok(())
    }
}
