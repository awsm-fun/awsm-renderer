//! Light culling render pass execution.
//!
//! Dispatches the cull compute shader once per frame: one workgroup per
//! `(tile_x, tile_y, z_slice)` froxel. Each workgroup zeroes its
//! per-froxel count and atomic-appends any punctual light whose world-
//! space bounding sphere overlaps the froxel's view-space frustum.
//!
//! Dispatch is skipped when there are no active punctual lights — the
//! shading passes then take their existing per-mesh / flat-loop paths,
//! which are correct (no froxel reads) even though the cull output
//! buffers stay stale.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        light_culling::{bind_group::LightCullingBindGroups, pipeline::LightCullingPipelines},
        RenderPassInitContext,
    },
};

/// Light culling pass bind groups and pipelines.
pub struct LightCullingRenderPass {
    pub bind_groups: LightCullingBindGroups,
    pub pipelines: LightCullingPipelines,
}

impl LightCullingRenderPass {
    /// Creates the light culling render pass resources. Eager compile
    /// — matches the existing scaffold and every other compute pass in
    /// this codebase. The cull dispatch itself is gated on
    /// `live_punctual_count > 0` at frame time, so the work is paid
    /// only when there's something to cull.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = LightCullingBindGroups::new(ctx).await?;
        let pipelines = LightCullingPipelines::new(ctx, &bind_groups).await?;
        Ok(Self {
            bind_groups,
            pipelines,
        })
    }

    /// Executes the light culling pass. Skipped when no punctual
    /// lights are live this frame — the consumer shaders observe
    /// stale counts in that case, but they also skip the froxel loop
    /// (transparent: walks `n_lights` from `lights_info` which is 0;
    /// opaque-oversized: the sentinel routing is bypassed for the
    /// no-light common case).
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let buffers = ctx.light_culling_buffers;
        let live_punctual = ctx.live_punctual_light_count();
        if live_punctual == 0 {
            return Ok(());
        }
        let bind_group = self.bind_groups.get_bind_group()?;
        let pipeline = ctx.pipelines.compute.get(self.pipelines.pipeline_key)?;

        let compute_pass = ctx
            .command_encoder
            .begin_compute_pass(Some(&ComputePassDescriptor::new(Some("Light Culling")).into()));
        compute_pass.set_pipeline(pipeline);
        compute_pass.set_bind_group(0, bind_group, None)?;
        compute_pass.dispatch_workgroups(
            buffers.tiles_x(),
            Some(buffers.tiles_y()),
            Some(buffers.slice_count),
        );
        compute_pass.end();

        Ok(())
    }
}
