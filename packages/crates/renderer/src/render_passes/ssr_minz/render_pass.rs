//! SSR min-Z pyramid build render pass execution.
//!
//! Two-step build (mirrors `hzb::render_pass`): seed (depth → mip 0), then a
//! reduce dispatch per mip level (`1..mip_count`). All coalesced into a single
//! compute pass. Each reduce dispatch is sized to the destination mip's
//! dimensions / 8, rounded up. The result is the min-Z depth pyramid the SSR
//! trace descends to skip empty space.

use awsm_renderer_core::{
    command::compute_pass::ComputePassDescriptor, renderer::AwsmRendererWebGpu,
};

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        ssr_minz::{
            bind_group::SsrMinzBindGroups, pipeline::SsrMinzPipelines, texture::SsrMinzTexture,
        },
        RenderPassInitContext,
    },
};

pub struct SsrMinzRenderPass {
    pub bind_groups: SsrMinzBindGroups,
    pub pipelines: SsrMinzPipelines,
    /// The min-Z pyramid texture itself. Owned by the pass so resize logic
    /// stays local; `bind_groups.recreate` rebuilds against this, and the SSR
    /// trace binds `texture.view_all`.
    pub texture: SsrMinzTexture,
}

impl SsrMinzRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = SsrMinzBindGroups::new(ctx).await?;
        let pipelines = SsrMinzPipelines::new(ctx, &bind_groups).await?;
        // Allocate at a small initial size; the per-frame resize hook in
        // `render.rs` recreates against the live viewport before the first
        // dispatch.
        let texture = SsrMinzTexture::new(ctx.gpu, 1, 1)?;
        Ok(Self {
            bind_groups,
            pipelines,
            texture,
        })
    }

    /// Re-allocates the pyramid texture to match the current viewport.
    /// Returns `true` when a new texture was created — the caller marks the
    /// dependent bind groups dirty in that case.
    pub fn ensure_size(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        width: u32,
        height: u32,
    ) -> Result<bool> {
        if self.texture.width == width && self.texture.height == height {
            return Ok(false);
        }
        self.texture = SsrMinzTexture::new(gpu, width, height)?;
        Ok(true)
    }

    /// Builds the min-Z pyramid for the current frame:
    /// 1. Seed mip 0 from the depth buffer (min-across-samples under MSAA).
    /// 2. Reduce mip 0 → 1, 1 → 2, …, mip_count-2 → mip_count-1.
    ///
    /// Coalesced into a single compute pass exactly like `hzb::render_pass`.
    /// WebGPU inserts the storage-binding barrier between intra-pass dispatches
    /// that write-then-read the same texture, so reduce mip(N+1) sees what
    /// reduce mip(N) wrote.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("SSR MinZ Build")).into(),
        ));

        // Seed dispatch — depth → mip 0.
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.seed)?);
        compute_pass.set_bind_group(0, self.bind_groups.seed()?, None)?;
        let seed_x = self.texture.width.div_ceil(8);
        let seed_y = self.texture.height.div_ceil(8);
        compute_pass.dispatch_workgroups(seed_x, Some(seed_y), Some(1));

        // Reduce dispatches — mip 0→1, 1→2, …, N-2→N-1. All inside the same
        // pass; switch to the reduce pipeline once, then re-bind per mip.
        let reduce_pipeline = ctx.pipelines.compute.get(self.pipelines.reduce)?;
        compute_pass.set_pipeline(reduce_pipeline);
        for transition in 0..(self.texture.mip_count.saturating_sub(1)) as usize {
            compute_pass.set_bind_group(0, self.bind_groups.reduce_at(transition)?, None)?;
            let (dst_w, dst_h) = self.texture.mip_dims((transition + 1) as u32);
            let workgroups_x = dst_w.div_ceil(8);
            let workgroups_y = dst_h.div_ceil(8);
            compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));
        }

        compute_pass.end();
        Ok(())
    }
}
