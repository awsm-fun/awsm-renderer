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
//! additive composite over `composite`. The min-Z pyramid + Hi-Z descent and
//! the temporal reprojection are built but gated off pending plan 004's
//! promote-or-delete decision.

pub mod bind_group;
pub mod composite;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
