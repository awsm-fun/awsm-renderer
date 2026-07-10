//! Screen-space reflections (SSR) pass.
//!
//! A stochastic, roughness-aware (via a material-owned reflection descriptor),
//! Hi-Z-traced reflection pass. Modeled on the `bloom`
//! pass: self-contained (own bind groups + pipelines + params + reflection
//! target), inserted after the HZB build and before the transparent pass, and
//! compiled granularly so `enabled = false` records + allocates nothing (§5a).
//!
//! Staged: M1 = mirror reflections via a view-space linear DDA march on the
//! existing depth/normal/HDR buffers (no new G-buffer target); M2 adds the
//! min-Z pyramid + glossy GGX path + the material reflection descriptor; M3 the
//! temporal + spatial denoise.

pub mod bind_group;
pub mod composite;
pub mod pipeline;
pub mod render_pass;
pub mod shader;
