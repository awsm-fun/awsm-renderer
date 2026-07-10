//! SSR min-Z (nearest-depth) hierarchical-depth pyramid build pass.
//!
//! A dedicated min-reduced depth pyramid for the SSR trace's Hi-Z ray march
//! (M2c — Hi-Z min-Z pyramid). Structurally a mirror of the occlusion HZB pass
//! (`render_passes/hzb/`) but flipped in two ways:
//! - It stores **minimum** depth per tile (the NEAREST surface), because a
//!   reflection ray needs the closest potential occluder — whereas the
//!   occlusion HZB stores max depth.
//! - It is gated on **`post_processing.ssr.enabled`**, not `features.gpu_culling`
//!   (SSR runs in scenes without GPU culling, so the occlusion HZB is often
//!   absent exactly when SSR needs a pyramid).
//!
//! Built after the opaque pass (depth final) and before the SSR trace dispatch;
//! the trace binds `texture.view_all` and descends it to skip empty space,
//! producing reflections identical to the linear-DDA fallback but faster.
//!
//! Simpler than the occlusion HZB: no MSAA lazy-pool (one seed variant matching
//! the live AA / the SSR trace's depth binding), self-contained pipelines.

pub mod bind_group;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
pub mod texture;
