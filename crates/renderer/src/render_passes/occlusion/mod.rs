//! GPU occlusion culling.
//!
//! Per-frame flow:
//! 1. CPU walks `renderables.opaque`, packs each into an
//!    [`buffers::OcclusionInstance`] (world AABB + meta offset),
//!    writes the array into the GPU instance buffer.
//! 2. Compute shader at `workgroup_size(64)` runs one thread per
//!    instance: frustum-test against camera planes, HZB-test against
//!    `hzb_tex` at the appropriate mip, writes 0/1 to
//!    `visible_this_frame[i]`.
//! 3. The compaction pass consumes `visible_this_frame` to gate
//!    `drawIndirect.instance_count`.

pub mod bind_group;
pub mod buffers;
pub mod compaction;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
