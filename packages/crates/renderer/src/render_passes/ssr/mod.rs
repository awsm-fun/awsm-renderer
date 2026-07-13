//! Screen-space reflections (SSR) pass.
//!
//! A roughness-aware (via a material-owned reflection descriptor) reflection
//! pass. Modeled on the `bloom` pass: self-contained (own bind groups +
//! pipelines + params + reflection target), it runs AFTER the transparent
//! pass / MSAA resolve (so the HDR color source is the resolved single-sample
//! `composite`) and BEFORE bloom, and is compiled granularly so
//! `enabled = false` records + allocates nothing (§5a).
//!
//! Shipped path: view-space linear-DDA trace into the (half-res by default)
//! `ssr` target as reflection-only premultiplied color, then an edge-aware
//! additive composite over `composite`. Plan 004 Part 2 deleted the dormant
//! Hi-Z (min-Z pyramid) acceleration wholesale — linear DDA is the one and
//! only trace strategy.

pub mod bind_group;
pub mod composite;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
