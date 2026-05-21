//! HZB build render pass execution — Cluster 7.1, plan §16.6.
//!
//! Two-step build: seed (depth → mip 0), then a reduce dispatch per
//! mip level (`1..mip_count`). Each reduce dispatch is sized to the
//! destination mip's dimensions / 8, rounded up.

use awsm_renderer_core::{
    command::compute_pass::ComputePassDescriptor, renderer::AwsmRendererWebGpu,
};

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        hzb::{bind_group::HzbBindGroups, pipeline::HzbPipelines, texture::HzbTexture},
        RenderPassInitContext,
    },
};

pub struct HzbRenderPass {
    pub bind_groups: HzbBindGroups,
    pub pipelines: HzbPipelines,
    /// The HZB texture itself. Owned by the pass so resize logic
    /// stays local; `bind_groups.recreate` rebuilds against this.
    pub texture: HzbTexture,
}

impl HzbRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = HzbBindGroups::new(ctx).await?;
        let pipelines = HzbPipelines::new(ctx, &bind_groups).await?;
        // Allocate at a small initial size; the per-frame resize hook
        // in `render.rs` recreates against the live viewport before
        // the first dispatch.
        let texture = HzbTexture::new(ctx.gpu, 1, 1)?;
        Ok(Self {
            bind_groups,
            pipelines,
            texture,
        })
    }

    /// Re-allocates the HZB texture to match the current viewport.
    /// Returns `true` when a new texture was created — the caller
    /// marks the dependent bind groups dirty in that case.
    pub fn ensure_size(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        width: u32,
        height: u32,
    ) -> Result<bool> {
        if self.texture.width == width && self.texture.height == height {
            return Ok(false);
        }
        self.texture = HzbTexture::new(gpu, width, height)?;
        Ok(true)
    }

    /// Builds the HZB for the current frame:
    /// 1. Seed mip 0 from the depth buffer.
    /// 2. Reduce mip 0 → 1, 1 → 2, …, mip_count-2 → mip_count-1.
    ///
    /// Each reduce dispatch is sized to its destination-mip dimensions
    /// / 8, with the per-thread bounds check inside the WGSL handling
    /// the leftover when dimensions aren't a multiple of 8.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        // Seed pass.
        {
            let compute_pass = ctx
                .command_encoder
                .begin_compute_pass(Some(&ComputePassDescriptor::new(Some("HZB Seed")).into()));
            let pipeline_key = if ctx.anti_aliasing.msaa_sample_count.is_some() {
                self.pipelines.seed_msaa
            } else {
                self.pipelines.seed_single
            };
            compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
            compute_pass.set_bind_group(0, self.bind_groups.seed()?, None)?;
            let workgroups_x = self.texture.width.div_ceil(8);
            let workgroups_y = self.texture.height.div_ceil(8);
            compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));
            compute_pass.end();
        }

        // Reduce pass per mip transition.
        for transition in 0..(self.texture.mip_count.saturating_sub(1)) as usize {
            let compute_pass = ctx
                .command_encoder
                .begin_compute_pass(Some(&ComputePassDescriptor::new(Some("HZB Reduce")).into()));
            compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.reduce)?);
            compute_pass.set_bind_group(0, self.bind_groups.reduce_at(transition)?, None)?;
            let (dst_w, dst_h) = self.texture.mip_dims((transition + 1) as u32);
            let workgroups_x = dst_w.div_ceil(8);
            let workgroups_y = dst_h.div_ceil(8);
            compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));
            compute_pass.end();
        }

        Ok(())
    }
}
