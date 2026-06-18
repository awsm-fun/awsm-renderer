//! Projection decals.
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

use awsm_renderer_core::renderer::AwsmRendererWebGpu;

/// Stride used to pack a decal's flat `texture_index` into the texture pool's
/// `(array_index, layer_index)` — `texture_index = array_index * stride +
/// layer_index`, unpacked in `material_decal_wgsl/compute.wgsl` as
/// `layer = index % stride`, `array_index = index / stride`.
///
/// **Single source of truth (A.4).** This MUST equal the divisor the decal shader
/// is templated with — both call this same function, sourcing the device
/// `max_texture_array_layers`. The texture pool fills each `(w,h,format)` array up
/// to that many layers ([`crate::textures`]), so `layer_index < stride` always and
/// any decal texture round-trips. (It used to be a hard-coded `64` duplicated in
/// the shader and the scene-loader — a decal on `layer_index >= 64` sampled the
/// wrong texture once a pool array exceeded 64 layers.)
pub fn decal_texture_index_stride(gpu: &AwsmRendererWebGpu) -> u32 {
    gpu.device.limits().max_texture_array_layers()
}
