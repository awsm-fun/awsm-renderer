//! Light culling render pass execution.
//!
//! Dispatches the cull compute shader once per frame: one workgroup per
//! `(tile_x, tile_y, z_slice)` froxel. Each workgroup zeroes its
//! per-froxel count and atomic-appends any punctual light whose world-
//! space bounding sphere overlaps the froxel's view-space frustum.
//!
//! Dispatch is skipped only when there are **no lights at all** this
//! frame. It is *not* skipped on no-punctual / directional-only scenes:
//! the froxel consumers walk the per-froxel slices whenever
//! `lights_info.n_lights > 0`, and the cull pass is the sole
//! writer/clearer of those counts, so skipping it there would leave
//! stale froxel data. See `render()` for the full rationale.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::light_culling::{
        bind_group::LightCullingBindGroups, pipeline::LightCullingPipelines,
    },
};

/// Light culling pass bind groups and pipelines. Constructed through the
/// staged `describe → from_resolved` flow in `render_passes.rs` — bind
/// groups in phase 1, the two pipelines through the cross-renderer pool.
/// The cull dispatch itself is gated on `live_light_count() > 0` at frame
/// time (see `render()`): it must run for directional-only scenes too, to
/// clear the froxel counts the consumers still read; only truly light-free
/// frames skip it.
pub struct LightCullingRenderPass {
    pub bind_groups: LightCullingBindGroups,
    pub pipelines: LightCullingPipelines,
}

impl LightCullingRenderPass {
    /// Executes the light culling pass.
    ///
    /// The dispatch may only be skipped when there are **no lights at
    /// all** this frame. It is *not* enough to skip when there are no
    /// punctual lights: the froxel consumers (transparent always,
    /// opaque-oversized via the `0xFFFFFFFF` sentinel) walk the
    /// per-froxel slices whenever `lights_info.n_lights > 0` — which is
    /// true for directional-only scenes too. Since the cull pass is the
    /// sole writer/clearer of the per-tile/per-froxel counts, skipping
    /// it while consumers still read would leave stale counts from a
    /// prior frame and re-apply removed punctual lights.
    ///
    /// With at least one light present we always dispatch; `cs_tile` /
    /// `cs_main` cheaply clear the counts and skip directionals, so a
    /// directional-only frame just zeroes the froxel slices.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let buffers = ctx.light_culling_buffers;
        if ctx.live_light_count() == 0 {
            return Ok(());
        }
        let bind_group = self.bind_groups.get_bind_group()?;
        let tile_pipeline = ctx
            .pipelines
            .compute
            .get(self.pipelines.tile_pipeline_key)?;
        let froxel_pipeline = ctx.pipelines.compute.get(self.pipelines.pipeline_key)?;
        let tiles_x = buffers.tiles_x();
        let tiles_y = buffers.tiles_y();

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Light Culling")).into(),
        ));
        compute_pass.set_bind_group(0, bind_group, None)?;
        // Stage A — per-2D-tile side-plane cull (one workgroup per tile,
        // Z-independent). Writes each tile's candidate light list.
        compute_pass.set_pipeline(tile_pipeline);
        compute_pass.dispatch_workgroups(tiles_x, Some(tiles_y), Some(1));
        // Stage B — per-froxel Z-refine. Reads each froxel's tile
        // candidate list (written by Stage A above; WebGPU inserts the
        // read-after-write storage barrier between dispatches in the same
        // pass) and applies only the cheap Z-slice test.
        compute_pass.set_pipeline(froxel_pipeline);
        compute_pass.dispatch_workgroups(tiles_x, Some(tiles_y), Some(buffers.slice_count));
        compute_pass.end();

        Ok(())
    }
}
