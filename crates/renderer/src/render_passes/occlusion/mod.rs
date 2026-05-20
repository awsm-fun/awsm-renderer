//! GPU occlusion culling — Cluster 7.2 / plan §16.7 Phase 1.
//!
//! Phase 1 lands the *infrastructure* — the per-instance storage
//! buffer, the cull compute pass, and the render-graph slot — but
//! does NOT consume the cull output yet. The geometry pass keeps its
//! existing CPU-recorded `draw_indexed` loop driven by the BVH-visible
//! renderables list. The cull pass writes 0/1 into `visible_this_frame`
//! per instance; a future Phase 2 + §16.8 split the geometry pass into
//! "last-frame survivors" + "newly visible" halves and rewires draws
//! through `drawIndirect`.
//!
//! Per-frame flow:
//! 1. CPU walks `renderables.opaque`, packs each into an
//!    [`buffers::OcclusionInstance`] (world AABB + meta offset),
//!    writes the array into the GPU instance buffer.
//! 2. Compute shader at `workgroup_size(64)` runs one thread per
//!    instance: frustum-test against camera planes, HZB-test against
//!    `hzb_tex` at the appropriate mip, writes 0/1 to
//!    `visible_this_frame[i]`.
//! 3. (Phase 2) The geometry-pass split consumes
//!    `visible_this_frame` to gate `drawIndirect.instance_count`.

pub mod bind_group;
pub mod buffers;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
