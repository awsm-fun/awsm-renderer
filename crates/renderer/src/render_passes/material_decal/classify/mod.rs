//! Per-tile decal classify pass (§16.4.C).
//!
//! For each active decal, project its world-space AABB to screen
//! coordinates and atomic-append the decal's index to the bucket of
//! every 8×8 tile it overlaps. The decal shading compute then iterates
//! *only* the bucketed indices for its tile instead of the full
//! global decal list, lifting the per-pixel cost from `O(decals)` to
//! `O(decals_overlapping_tile)`.
//!
//! v1 ships *frustum* gating (decals fully behind / beside the camera
//! never enter any bucket). The §16.4.C spec also calls for HZB
//! occlusion gating — a per-tile depth read that drops decals whose
//! closest-screen-depth sits behind the HZB lookup. That's a 30-line
//! follow-up to the classify shader once we want it; the bucket layout
//! / dispatch shape don't change.

pub mod bind_group;
pub mod buffers;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
