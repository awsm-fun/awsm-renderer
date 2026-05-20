//! Material classify compute pass — produces per-`shader_id` tile
//! buckets + indirect-dispatch args consumed by the opaque material
//! pipelines (Cluster 6.1, plan §16.3.B).

pub mod bind_group;
pub mod buffers;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
