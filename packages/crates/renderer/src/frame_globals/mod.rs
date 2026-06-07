//! Renderer-wide per-frame state — `time`, `delta_time`, `frame_count`,
//! `resolution`.
//!
//! Lives alongside the camera uniform but stays a separate concept: these
//! values aren't camera properties (shadow / post-fx / picture-in-picture
//! passes each have their own camera; renderer-wide time is shared) and
//! deserve their own discoverable surface.

pub mod globals;
pub mod snapshot;

pub use globals::{AwsmFrameGlobalsError, FrameGlobals, DELTA_TIME_CLAMP_SECS};
pub use snapshot::FrameGlobalsSnapshot;
