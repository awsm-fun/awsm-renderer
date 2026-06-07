use crate::{camera::CameraMatrices, AwsmRenderer};

impl AwsmRenderer {
    /// Convenience helper to update non-GPU properties once per frame.
    ///
    /// Pair this with `render()` for a simple frame loop; for physics-heavy scenes,
    /// you may want to update transforms more frequently. `global_time_delta_ms`
    /// is the frame delta in **milliseconds** (a rAF timestamp difference);
    /// `update_animations` converts it to seconds internally.
    pub fn update_all(
        &mut self,
        global_time_delta_ms: f64,
        camera_matrices: CameraMatrices,
    ) -> crate::error::Result<()> {
        self.update_animations(global_time_delta_ms)?;
        // `update_transforms` owns all per-frame renderer-side
        // bookkeeping (frame_index bump, BVH refit, light-bucket
        // rebuild, shadow-receiver flagging, debug invariants). The
        // editor calls `update_transforms` directly without going
        // through `update_all`; keeping the work centralised in
        // `update_transforms` keeps both paths in lockstep.
        self.update_transforms();
        self.update_camera(camera_matrices)?;

        Ok(())
    }
}
