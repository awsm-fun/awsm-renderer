//! GPU mesh-pixel-coverage producer.
//!
//! Tiny compute pass run after the geometry pass. One thread per
//! pixel reads the visibility buffer, recovers
//! `mesh_meta_offset / 256` to get the per-mesh slot, and atomicAdds
//! 1 into `mesh_pixel_counts[slot]`. The CPU asynchronously reads
//! back the counts and routes them into
//! [`crate::coverage::MeshCoverage`], which downstream consumers
//! (skinning-skip in `meshes/skins.rs`, cheap-material LOD in
//! `collect_renderables`) already consult.
//!
//! Latency: counts arrive one frame after their producer dispatched —
//! the renderer's `MeshCoverage::is_visible_last_frame` API name is
//! exact. Conservative default for "not yet readable" entries is
//! "visible" so a fresh boot doesn't drop new geometry.

pub mod bind_group;
pub mod buffers;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
