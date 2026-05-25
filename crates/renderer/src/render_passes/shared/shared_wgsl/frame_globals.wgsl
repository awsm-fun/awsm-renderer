// Renderer-wide per-frame uniform — `time`, `delta_time`,
// `frame_count`, `resolution`. CPU source lives in
// `crates/renderer/src/frame_globals`; written once per `render()` call
// and bound alongside the camera uniform in every pass that pulls
// camera (the two have identical lifetimes — one per frame, written by
// the renderer, read everywhere).
//
// Each pass declares its own `@group(N) @binding(M) var<uniform>
// frame_globals_raw: FrameGlobalsRaw;` line in its `bind_groups.wgsl`
// (mirroring how `camera_raw` is declared). This file only defines the
// structs + the `frame_globals_from_raw` helper.

// Raw uniform layout — matches the 32-byte GPU buffer packed by
// `FrameGlobals::write_gpu`:
//   0   time         : f32
//   4   delta_time   : f32
//   8   frame_count  : u32
//  12   _pad         : u32
//  16   resolution.x : u32
//  20   resolution.y : u32
//  24   _pad2.x      : u32
//  28   _pad2.y      : u32
struct FrameGlobalsRaw {
    time: f32,
    delta_time: f32,
    frame_count: u32,
    _pad: u32,
    resolution: vec2<u32>,
    _pad2: vec2<u32>,
};

// Friendly view — same fields, omits `_pad` and `_pad2` (the
// alignment-only words that exist in `FrameGlobalsRaw`).
struct FrameGlobals {
    /// Seconds since renderer construction (or whatever value
    /// `AwsmRenderer::set_time_source` injected). Monotonic in the
    /// default wall-clock mode. `f32` is fine for sessions up to a few
    /// hours; very long sessions degrade precision visibly — see the
    /// rationale on `FrameGlobals::write_gpu`.
    time: f32,
    /// Seconds since the previous `render()` call. First frame after
    /// construction reports `0.0`. Upper-clamped at 0.25 to keep
    /// integrators stable across long tab-backgrounded pauses. No lower
    /// clamp — pumping the same time twice via `set_time_source`
    /// reports `0.0`, the right answer for paused gameplay.
    delta_time: f32,
    frame_count: u32,
    /// Renderer output resolution in pixels, `(width, height)`.
    resolution: vec2<u32>,
};

fn frame_globals_from_raw(raw: FrameGlobalsRaw) -> FrameGlobals {
    return FrameGlobals(raw.time, raw.delta_time, raw.frame_count, raw.resolution);
}
