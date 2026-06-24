//! Cluster-LOD (`virtual_geometry`) GPU render pass (Phase B, B.2).
//!
//! Evaluates the per-cluster LOD cut on-device — the GPU form of the CPU
//! reference [`crate::cluster_lod::select_cut_per_cluster`]. Gated by the
//! `virtual_geometry` feature; inert (and byte-identical to today) when off.
//!
//! The compute shader, its bind group, pipeline, and buffers are being built
//! incrementally (see docs/plans/lod.md, B.2-GPU). This module currently hosts
//! the registered cut shader; the pipeline + dispatch + readback follow.

pub mod buffers;
pub mod shader;
