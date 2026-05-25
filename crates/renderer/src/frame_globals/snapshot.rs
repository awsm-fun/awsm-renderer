//! [`FrameGlobalsSnapshot`] — CPU-side mirror of the values uploaded to
//! the `frame_globals` uniform this frame.

/// Snapshot of the values the renderer wrote into the `frame_globals`
/// uniform on the current frame. Cheap to copy.
///
/// Returned by [`crate::AwsmRenderer::frame_globals`]; the canonical
/// surface is the `frame_globals` uniform inside shaders. The CPU-side
/// view exists so subsystems running their own per-frame ticks (particle
/// simulators, gameplay clocks, animation drivers) can read the same
/// `delta_time` the GPU sees rather than rolling their own
/// `performance.now()` math.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameGlobalsSnapshot {
    /// Seconds since renderer construction (or whatever value the most
    /// recent [`crate::AwsmRenderer::set_time_source`] call injected).
    /// Monotonically increasing in the default wall-clock mode.
    pub time: f32,
    /// Seconds since the previous `render()` call.
    ///
    /// First frame after construction reports `0.0` (no prior frame to
    /// subtract from). Subsequent frames upper-clamp at
    /// [`super::DELTA_TIME_CLAMP_SECS`] to keep per-frame integrators
    /// stable after long backgrounded-tab pauses. No lower clamp —
    /// pumping the same time twice via `set_time_source` reports
    /// `delta_time == 0.0`, the right answer for paused gameplay.
    pub delta_time: f32,
    /// Monotonic frame counter. Mirrors
    /// `render_textures.frame_count()` — that's the canonical CPU
    /// source of truth; `FrameGlobals` just re-publishes it for
    /// shader-side access.
    pub frame_count: u32,
    /// Renderer output resolution in pixels, `(width, height)`. Useful
    /// for screen-space effects that don't care which camera produced
    /// the frame (post-FX, full-screen overlays).
    pub resolution: [u32; 2],
}
