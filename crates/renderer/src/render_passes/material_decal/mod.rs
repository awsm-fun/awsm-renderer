//! Projection-decal compute pass (Cluster 6.4, plan §16.4).
//!
//! Runs once per frame after the opaque material pipelines have
//! settled `opaque_tex`. Per pixel: reconstruct world position from
//! depth, iterate every active decal, and alpha-blend the decal's
//! sampled texel into the existing `opaque_tex` value. Skipped
//! entirely on frames with no active decals.

pub mod bind_group;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
