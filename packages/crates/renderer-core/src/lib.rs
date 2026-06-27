//! Core WebGPU wrappers, descriptors, and helpers for awsm-renderer.

pub mod alignment;
pub mod bind_groups;
pub mod brdf_lut;
pub mod buffers;
pub mod command;
pub mod compare;
pub mod compatibility;
pub mod configuration;
pub mod cubemap;
pub mod data;
pub mod error;
pub mod image;
pub mod keys;
pub mod methods;
pub mod pipeline;
pub mod renderer;
pub mod sampler;
pub mod shaders;
pub mod texture;
pub mod web_global;

/// Soft diagnostic threshold: any single GPU allocation larger than this is
/// flagged as a likely runaway size computation (overflow / flipped count) and
/// logged + `debug_assert!`ed at our call site. The allocation still proceeds —
/// this only surfaces "getting suspiciously big" early in dev. 512 MB is far
/// past any legitimate single buffer in this renderer. Only consulted under
/// `debug_assertions` / the `harden-diag` feature. See [`MAX_GPU_BUFFER_BYTES`]
/// for the always-on hard cap that actually prevents the crash.
pub const OVERSIZED_ALLOC_BYTES: u64 = 512 * 1024 * 1024;

/// Always-on hard ceiling for a single GPU buffer allocation. A request at or
/// above this is rejected as a recoverable `Err` *before* it reaches the WebGPU
/// API, instead of being passed through to abort the whole renderer process.
///
/// Chrome's PartitionAlloc traps with a deliberate `IMMEDIATE_CRASH`
/// (`EXC_BREAKPOINT`, the "Aw, Snap!") on any single allocation that reaches its
/// ~2 GiB `MaxDirectMapped` ceiling — observed in the wild as renderer aborts
/// with a `0x80000000` (2 GiB) size operand. That trap kills the process; it is
/// **not** a catchable WebGPU validation error. We cap a comfortable margin
/// below 2 GiB so a runaway buffer size fails the model/scene load gracefully
/// (propagated `Result`) rather than taking the editor/viewer down with it. No
/// legitimate single buffer here comes anywhere near this.
pub const MAX_GPU_BUFFER_BYTES: u64 = 1900 * 1024 * 1024;
