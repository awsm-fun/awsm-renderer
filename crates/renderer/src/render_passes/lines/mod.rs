//! Fat-line render pipeline.
//!
//! A polyline is uploaded as `positions: &[Vec3] + colors: &[Vec4]`, packed
//! into a storage buffer of N-1 `GpuLineSegment` records (each segment carries
//! its endpoints + per-endpoint colors). The vertex shader expands each segment
//! into a screen-space triangle strip whose perpendicular offset is a fixed
//! pixel width, giving true 1-3px GPU widths without geometry-shader hacks.
//!
//! Each line owns its own storage buffer, uniform buffer (viewport + width),
//! and bind group. Per frame, [`render_lines`] re-writes the uniform buffer
//! with the current viewport size, then issues one draw call per line
//! (4 vertices × N-1 instances, `TriangleStrip` topology).
//!
//! Four pipeline variants cover the cross product
//! (`depth_compare = Less | Always`) × (`MSAA = on | off`).

mod api;
mod gpu;
pub mod pipelines;
mod renderer;
pub mod shader;
mod types;

pub use renderer::LineRenderer;
pub use shader::cache_key::ShaderCacheKeyLine;
pub use types::{LineKey, LineTopology};
