//! Per-tile decal classify pass.
//!
//! For each active decal, project its world-space AABB to screen
//! coordinates and atomic-append the decal's index to the bucket of
//! every 8×8 tile it overlaps. The decal shading compute then iterates
//! *only* the bucketed indices for its tile instead of the full
//! global decal list, lifting the per-pixel cost from `O(decals)` to
//! `O(decals_overlapping_tile)`.
//!
//! Frustum gating drops decals fully behind / beside the camera. When
//! `features.gpu_culling` is on, an additional HZB occlusion gate
//! drops decals whose closest-screen-depth sits behind the HZB lookup.

pub mod bind_group;
pub mod buffers;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
