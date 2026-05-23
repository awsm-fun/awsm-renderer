//! Material classify compute pass — produces per-`shader_id` tile
//! buckets + indirect-dispatch args consumed by the opaque material
//! pipelines.

pub mod bind_group;
pub mod buffers;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
