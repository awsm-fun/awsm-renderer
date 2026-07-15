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

/// Cumulative (monotonic, increment-only) census of `create_buffer` calls —
/// the memory-leak-soak instrument (see docs/plans/crashes.md). We deliberately
/// do NOT track *live* buffers: there is no central buffer-destroy chokepoint
/// (many `web_sys::GpuBuffer` handles are released by GC with no explicit
/// `.destroy()` at all), so a decrement keyed on the scattered destroy sites
/// would drift upward and manufacture a false "leak". A cumulative *creation*
/// count/bytes is unambiguous: its SLOPE over a soak run directly measures how
/// fast the render loop is minting GPU buffers. Cross-referenced with the OS
/// `vmmap` virtual size (the ground truth for retained VA), it disambiguates a
/// GPU-buffer leak (both climb together) from a wasm-heap / JS leak (vmmap
/// climbs, this stays flat). Surfaced through the editor `memory_stats` query.
pub static CREATE_BUFFER_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static CREATE_BUFFER_BYTES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// `(count, bytes)` of every `create_buffer` call since process start. Read by
/// the editor's `memory_stats` census for the leak soak; a rising slope on
/// either names the render loop as a per-frame buffer-minting source.
pub fn create_buffer_census() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        CREATE_BUFFER_COUNT.load(Relaxed),
        CREATE_BUFFER_BYTES.load(Relaxed),
    )
}

/// Cumulative counts of `create_bind_group` / `create_command_encoder` — the
/// other two per-frame GPU-object mint points the leak soak watches. `create_buffer`
/// being flat while one of these climbs at the region-leak rate names it as the
/// per-frame-churn source (bind groups pin the resources they reference; a
/// per-frame descriptor is the textbook WebGPU renderer-process growth).
pub static CREATE_BIND_GROUP_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static CREATE_COMMAND_ENCODER_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// `(bind_group, command_encoder)` cumulative create counts since process start.
pub fn create_object_census() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        CREATE_BIND_GROUP_COUNT.load(Relaxed),
        CREATE_COMMAND_ENCODER_COUNT.load(Relaxed),
    )
}
