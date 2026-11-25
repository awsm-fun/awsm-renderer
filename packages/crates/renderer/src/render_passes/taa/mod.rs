//! Temporal anti-aliasing resolve (plan `docs/plans/taa-upscaling`, 03 §3-§4).
//!
//! Self-contained like SSR / SMAA: own bind groups, pipeline, params uniform
//! and textures (history pair, output, previous depth). It runs right after
//! the SSR composite and before bloom (03 §6); in TAA mode bloom and effects
//! read its `out` instead of `composite`.
//!
//! Zero cost when off: the pass is `None` unless `anti_aliasing.taa` is
//! `Some`; enabling builds it (shader, pipeline, textures), disabling drops it
//! and destroys the textures.
//!
//! The config types, the jitter sequence and the reconstruction kernel live
//! in `crate::taa`; the resolve's WGSL in `shader/taa_wgsl/resolve.wgsl`.

pub mod bind_group;
pub mod dof_pre;
pub mod pipeline;
pub mod rcas;
pub mod reactive_seed;
pub mod render_pass;
pub mod shader;
pub mod texture;
