//! `FrameGlobals` uniform + the renderer-facing time-source accessors.
//! See the [`crate::frame_globals`] module docs for the surface + rationale.

use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
use awsm_renderer_core::error::AwsmCoreError;
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use thiserror::Error;

use super::snapshot::FrameGlobalsSnapshot;
use crate::buffer::mapped_uploader::MappedUploader;
use crate::{AwsmRenderer, AwsmRendererLogging};

/// Per-frame upper-clamp on `delta_time`. Keeps integrators stable after a
/// long backgrounded-tab pause. No lower clamp — a consumer that pumps the
/// same `set_time_source` value twice gets `delta_time == 0.0`, which is
/// the right answer for paused gameplay.
pub const DELTA_TIME_CLAMP_SECS: f32 = 0.25;

/// Renderer-wide per-frame uniform.
///
/// 32-byte uniform buffer (`time`, `delta_time`, `frame_count`, `_pad`,
/// `resolution: vec2<u32>`, `_pad2: vec2<u32>`), updated once per
/// `render()` call and bound to every shader that already sees the camera
/// uniform.
pub struct FrameGlobals {
    pub gpu_buffer: web_sys::GpuBuffer,
    raw_data: [u8; Self::BYTE_SIZE],
    uploader: MappedUploader,

    /// `Performance.now()` reading captured at renderer construction.
    /// Kept in `f64` so millisecond precision survives long sessions; the
    /// uniform stays `f32` for shader simplicity.
    construction_ms: f64,
    /// Last frame's `time` value (in seconds). `None` before the first
    /// `write_gpu` call — that frame reports `delta_time = 0.0`.
    last_time: Option<f32>,
    /// Optional time-source override set via
    /// [`AwsmRenderer::set_time_source`]. Cleared by callers when they
    /// want to return control to the wall-clock source.
    time_override: Option<f32>,

    /// Cached `(time, delta_time, frame_count, resolution)` snapshot — the
    /// values that landed in the uniform at the most recent `write_gpu`.
    /// Surfaced via [`AwsmRenderer::frame_globals`] so CPU-side consumers
    /// (particle simulators, gameplay clocks) read the same numbers the
    /// shaders see this frame.
    snapshot: FrameGlobalsSnapshot,
}

impl FrameGlobals {
    /// Byte size of the uniform — 32 bytes, see module docs.
    pub const BYTE_SIZE: usize = 32;

    /// Allocate the GPU buffer and capture the construction-time
    /// `Performance.now()` reading.
    pub fn new(gpu: &AwsmRendererWebGpu) -> std::result::Result<Self, AwsmFrameGlobalsError> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("FrameGlobals"),
                Self::BYTE_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;

        let construction_ms = performance_now_ms();

        Ok(Self {
            gpu_buffer,
            raw_data: [0; Self::BYTE_SIZE],
            uploader: MappedUploader::new("FrameGlobals"),
            construction_ms,
            last_time: None,
            time_override: None,
            snapshot: FrameGlobalsSnapshot {
                time: 0.0,
                delta_time: 0.0,
                frame_count: 0,
                resolution: [0, 0],
            },
        })
    }

    /// Mapped-ring upload telemetry for the frame-globals buffer.
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.uploader.stats()
    }

    /// Current snapshot — the values shaders will see this frame. Updated
    /// at the top of `write_gpu`; safe to read for the duration of the
    /// current frame.
    pub fn snapshot(&self) -> FrameGlobalsSnapshot {
        self.snapshot
    }

    /// Overrides the time source — see
    /// [`AwsmRenderer::set_time_source`] for semantics.
    pub fn set_time_source(&mut self, time: f32) {
        self.time_override = Some(time);
    }

    /// The current pinned time, if a [`set_time_source`](Self::set_time_source)
    /// override is active (else `None` = wall-clock).
    pub fn time_source(&self) -> Option<f32> {
        self.time_override
    }

    /// Drop the time-source override; subsequent frames go back to the
    /// `Performance.now()` wall clock.
    pub fn clear_time_source(&mut self) {
        self.time_override = None;
    }

    /// Pack the per-frame values and upload via the mapped-ring path.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        frame_count: u32,
        resolution: [u32; 2],
    ) -> std::result::Result<(), AwsmFrameGlobalsError> {
        let _maybe_span_guard = if logging.render_timings.sub_frame() {
            Some(tracing::span!(tracing::Level::INFO, "FrameGlobals GPU write").entered())
        } else {
            None
        };

        // Resolve `time`: override wins; otherwise compute from
        // wall-clock minus the construction-time reference.
        let time = match self.time_override {
            Some(t) => t,
            None => {
                let elapsed_ms = performance_now_ms() - self.construction_ms;
                (elapsed_ms / 1000.0) as f32
            }
        };

        // First frame has no prior `last_time` to subtract from →
        // `delta_time = 0.0`. Subsequent frames upper-clamp at
        // `DELTA_TIME_CLAMP_SECS` (no lower clamp; `0.0` is valid for
        // paused-clock scenarios).
        let delta_time = match self.last_time {
            Some(prev) => (time - prev).clamp(0.0, DELTA_TIME_CLAMP_SECS),
            None => 0.0,
        };
        self.last_time = Some(time);

        // Pack the 32-byte uniform:
        //   0   time           : f32
        //   4   delta_time     : f32
        //   8   frame_count    : u32
        //  12   _pad           : u32
        //  16   resolution.x   : u32
        //  20   resolution.y   : u32
        //  24   _pad2.x        : u32
        //  28   _pad2.y        : u32
        self.raw_data[0..4].copy_from_slice(&time.to_ne_bytes());
        self.raw_data[4..8].copy_from_slice(&delta_time.to_ne_bytes());
        self.raw_data[8..12].copy_from_slice(&frame_count.to_ne_bytes());
        self.raw_data[12..16].copy_from_slice(&0u32.to_ne_bytes());
        self.raw_data[16..20].copy_from_slice(&resolution[0].to_ne_bytes());
        self.raw_data[20..24].copy_from_slice(&resolution[1].to_ne_bytes());
        self.raw_data[24..28].copy_from_slice(&0u32.to_ne_bytes());
        self.raw_data[28..32].copy_from_slice(&0u32.to_ne_bytes());

        self.snapshot = FrameGlobalsSnapshot {
            time,
            delta_time,
            frame_count,
            resolution,
        };

        // Whole-buffer dirty range — `time` advances every frame, so the
        // packed bytes always differ from the previous frame's.
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

impl AwsmRenderer {
    /// Returns the current frame-globals snapshot — the values shaders
    /// see this frame (`time`, `delta_time`, `frame_count`, `resolution`).
    /// Safe to read for the duration of the current frame; refreshed at
    /// the top of every `render()` call.
    ///
    /// Most consumers don't interact with this — shader read access is
    /// the primary surface. The CPU-side accessor exists so subsystems
    /// running their own per-frame ticks (particle simulators, gameplay
    /// clocks) can read the same `delta_time` value the GPU sees
    /// without rolling their own `performance.now()` deltas.
    pub fn frame_globals(&self) -> FrameGlobalsSnapshot {
        self.frame_globals.snapshot()
    }

    /// Overrides the renderer's time source.
    ///
    /// By default the renderer derives `time` from `Performance.now()`
    /// (wall clock since `AwsmRenderer::new()` returned). Consumers
    /// running their own game-time clock — paused gameplay, bullet-time
    /// effects, replay systems — call this before each `render()` to
    /// inject the value the next frame's `frame_globals.time` will see.
    ///
    /// `delta_time` is always computed from successive `time` values
    /// regardless of source; passing the same `time` twice in a row
    /// reports `delta_time == 0.0`, which is the correct answer for a
    /// paused simulation.
    ///
    /// Call [`AwsmRenderer::clear_time_source`] to return to the
    /// wall-clock source.
    pub fn set_time_source(&mut self, time: f32) {
        self.frame_globals.set_time_source(time);
    }

    /// The current pinned frame time, if a `set_time_source` override is active
    /// (else `None` = wall-clock). Used to make UV flows deterministic when the
    /// time is pinned (§7).
    pub fn time_source(&self) -> Option<f32> {
        self.frame_globals.time_source()
    }

    /// Advance auto-scrolling texture UV flows once per frame (§7). When the time
    /// is PINNED (`set_frame_time`), pins each flow to that absolute time
    /// (`offset = base + velocity * t`) so a flow scroll is deterministic for
    /// temporal screenshots; otherwise integrates `dt_seconds` of real time. A
    /// no-op when nothing flows. The editor render loop and `update_animations`
    /// both call this so flows scroll on every render path.
    pub fn tick_texture_flows(&mut self, dt_seconds: f32) {
        match self.time_source() {
            Some(t) => self.textures.set_texture_flows_elapsed(t),
            None => self.textures.advance_texture_flows(dt_seconds),
        }
    }

    /// Stop overriding the time source; subsequent frames go back to
    /// reading `Performance.now()`.
    pub fn clear_time_source(&mut self) {
        self.frame_globals.clear_time_source();
    }
}

/// `Performance.now()` in milliseconds. Routes through
/// [`crate::web_global::performance`] so both main-thread and
/// worker-mode renderers read the same clock. Returns `0.0` if
/// `performance` isn't reachable.
fn performance_now_ms() -> f64 {
    crate::web_global::performance()
        .map(|p| p.now())
        .unwrap_or(0.0)
}

/// Frame-globals errors.
#[derive(Error, Debug)]
pub enum AwsmFrameGlobalsError {
    /// Core (WebGPU buffer create / upload) failure.
    #[error("[frame_globals] {0:?}")]
    Core(#[from] AwsmCoreError),
}
