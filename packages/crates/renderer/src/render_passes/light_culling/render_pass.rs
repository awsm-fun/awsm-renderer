//! Light culling render pass execution.
//!
//! Dispatches the cull compute shader once per frame: one workgroup per
//! `(tile_x, tile_y, z_slice)` froxel. Each workgroup zeroes its
//! per-froxel count and atomic-appends any punctual light whose world-
//! space bounding sphere overlaps the froxel's view-space frustum.
//!
//! Dispatch is skipped when there are **no lights at all** this frame, and
//! on no-punctual / directional-only frames once the counts are known to be
//! zero for the live grid. It can't simply be skipped on every
//! directional-only frame: the froxel consumers walk the per-froxel slices
//! whenever `lights_info.n_lights > 0`, and the cull pass is the sole
//! writer/clearer of those counts. See `render()` for the full rationale.

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
    /// The live `(tiles_x, tiles_y, slice_count)` grid whose froxel counts the
    /// last dispatch left all-zero (it ran with no punctual lights), or `None`
    /// when they may hold lights. While it matches the current grid and there
    /// are still no punctual lights, the dispatch would only re-zero zeros, so
    /// it's skipped. Keyed on the grid, not a bool: a viewport shrink keeps the
    /// buffers but shrinks the live grid, so counts outside it can be stale
    /// from an earlier, larger frame and come back into view on a regrow.
    ///
    /// Only updated by [`Self::commit_submitted`], after the frame's commands
    /// were actually submitted: a frame that errors out after recording the
    /// dispatch drops its encoder, so its counts never reached the GPU.
    pub clean_grid: std::cell::Cell<Option<(u32, u32, u32)>>,
    /// What [`Self::clean_grid`] becomes once this frame is submitted (`None`
    /// = no dispatch recorded this frame, nothing to commit).
    pub pending_clean_grid: std::cell::Cell<Option<Option<(u32, u32, u32)>>>,
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
    /// A directional-only frame's dispatch just zeroes the froxel slices
    /// (`cs_tile` / `cs_main` clear the counts and skip directionals). That is
    /// a full-screen grid of workgroups doing nothing useful once the counts
    /// ARE zero, so after one such dispatch it is skipped until a punctual
    /// light appears or the live grid changes (see [`Self::clean_grid`]).
    /// WebGPU zero-initializes new buffers, so a buffer recreate can't make a
    /// "clean" grid dirty.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let buffers = ctx.light_culling_buffers;
        // Discard anything a previous, never-submitted frame left pending.
        self.pending_clean_grid.set(None);
        if ctx.live_light_count() == 0 {
            return Ok(());
        }
        let grid = (buffers.tiles_x(), buffers.tiles_y(), buffers.slice_count);
        let no_punctual = ctx.live_punctual_count == 0;
        if no_punctual && self.clean_grid.get() == Some(grid) {
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
            &ComputePassDescriptor::new(Some("Light Culling"))
                .with_timestamp_writes_opt(
                    ctx.gpu_timestamps
                        .and_then(|t| t.writes_for_compute("Light Culling")),
                )
                .into(),
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
        self.pending_clean_grid
            .set(Some(no_punctual.then_some(grid)));

        Ok(())
    }

    /// Call right after the frame's command buffer is submitted: promotes the
    /// state this frame's dispatch (if any) left the counts in.
    pub fn commit_submitted(&self) {
        if let Some(clean) = self.pending_clean_grid.take() {
            self.clean_grid.set(clean);
        }
    }
}
