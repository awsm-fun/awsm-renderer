//! Hierarchical-Z (HZB) build pass — Cluster 7.1, plan §16.6.
//!
//! Builds a min/max-reduced mip chain from the final-resolution depth
//! buffer via successive compute passes. The result lives in
//! [`HzbTexture`] (an `r32float` mip chain) and is consumed by:
//! - Two-phase GPU occlusion culling (Cluster 7.2 / 16.7) — coarse
//!   reject of instances whose screen-space AABB sits behind the
//!   HZB lookup at their footprint mip.
//! - GPU instance compaction + indirect draw (Cluster 7.3 / 16.8).
//! - Decal-tile classification (§16.4.C) — gate decals by occluded
//!   tiles to skip the per-decal volume test where it can't matter.
//! - GPU coverage compute (Cluster 6.2) — derive a per-mesh
//!   "visible-pixel-count" estimate from HZB without an atomic-add
//!   per pixel.
//!
//! The HZB stores **maximum** depth per tile (the furthest-away
//! depth in each downsampled region). Occlusion test: a candidate is
//! occluded if its *closest* screen-space depth is greater than the
//! HZB lookup at the appropriate mip level — i.e. the candidate is
//! definitely behind everything in that region. Matches WebGPU's
//! `[0, 1]` depth convention where 1 = far.
//!
//! v1 has no consumer in tree yet — the pass is pure infrastructure.
//! Build it once a consumer needs it (16.7 will be the first).

pub mod bind_group;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
pub mod texture;
