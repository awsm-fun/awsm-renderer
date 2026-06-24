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

/// Opt-in hardening guard threshold: any single GPU allocation larger than this
/// is treated as a runaway size computation (overflow / flipped count) and
/// logged + `debug_assert!`ed at our call site rather than left to trap deep in
/// the browser allocator (the "Aw, Snap!" `IMMEDIATE_CRASH`). 512 MB is far past
/// any legitimate single buffer/texture in this renderer. Only consulted under
/// `debug_assertions` / the `harden-diag` feature.
pub const OVERSIZED_ALLOC_BYTES: u64 = 512 * 1024 * 1024;
