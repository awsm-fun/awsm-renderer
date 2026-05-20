//! Projection decals — Cluster 6.4, plan §16.4.
//!
//! Decima/D3-style oriented-unit-cube decals projecting their texture
//! down the local -Z axis onto whatever geometry sits inside the
//! cube's world-space volume. The runtime side owns the per-decal
//! data + GPU buffer; the render-graph side runs a `material_decal`
//! compute pass after the opaque material pass to overlay the
//! sampled decal color onto `opaque_tex`.
//!
//! v1 only supports alpha-blend (`final = lerp(opaque, decal.rgb,
//! decal.a * decal.alpha)`). Authoring extensions (additive,
//! multiply) are noted at the [`DecalBlendMode`] enum below.

mod api;
mod data;
mod gpu;

pub use data::{Decal, DecalBlendMode, DecalKey};
pub use gpu::{AwsmDecalError, Decals, MAX_DECAL_COUNT};
