//! Hierarchical-Z (HZB) build pass.
//!
//! Builds a max-reduced mip chain from the final-resolution depth
//! buffer via successive compute passes. The result lives in
//! [`texture::HzbTexture`] (an `r32float` mip chain) and is consumed by:
//! - GPU occlusion culling (`render_passes/occlusion/`) — coarse-
//!   reject instances whose screen-space AABB sits behind the HZB
//!   lookup at their footprint mip.
//! - Decal-tile classification
//!   (`render_passes/material_decal/classify/`) — skip decal
//!   tiles whose closest-screen-depth is behind the HZB max for
//!   the decal's screen-AABB footprint.
//!
//! The HZB stores **maximum** depth per tile (the furthest-away
//! depth in each downsampled region). Occlusion test: a candidate
//! is occluded if its *closest* screen-space depth is greater than
//! the HZB lookup at the appropriate mip level — i.e. the candidate
//! is definitely behind everything in that region. Matches WebGPU's
//! `[0, 1]` depth convention where 1 = far.
//!
//! Built only when `features.gpu_culling` is on; see `features.rs`.

pub mod bind_group;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
pub mod texture;
