//! Abstraction over the renderer's texture pool / sampler set for material
//! uniform-buffer writers.
//!
//! Materials need a handful of texture/sampler lookups when packing their
//! payload — they don't need the full `Textures` struct from `awsm-renderer`.
//! Routing those lookups through this trait keeps `awsm-renderer-materials` from
//! depending on `awsm-renderer`.

use awsm_renderer_core::{
    keys::{SamplerKey, TextureKey, TextureTransformKey},
    sampler::AddressMode,
    texture::texture_pool::{TexturePoolArray, TexturePoolEntryInfo},
};

/// Which shared 1×1 NEUTRAL a built-in material slot packs when no image is
/// bound. The five core PBR slots always compile their sampling path, so an
/// unbound slot must still resolve to a real pool entry; sampling the neutral
/// reproduces glTF's defined no-texture result exactly (white = identity
/// multiply; flat normal = the geometry normal through the TBN math).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeutralTexture {
    /// 1×1 white — base color / metallic-roughness / occlusion / emissive.
    White,
    /// 1×1 flat normal, packed rgba8 `[128, 128, 255]` — the same encoding
    /// (and the same ~0.2° quantization off exact `(0.5, 0.5, 1.0)`) that every
    /// authored normal map carries in its flat regions. Unpacks to tangent
    /// `≈(0, 0, 1)`, so `TBN · tangent ≈` the geometry normal. Note the residual
    /// `128/255 ≈ 0.502` on x/y is scaled by `normal_scale`, so a large
    /// `normal_scale` with no bound normal map tilts the normal very slightly.
    FlatNormal,
}

/// Renderer-side context that lets a material's `write_uniform_buffer` resolve
/// pooled texture / sampler / texture-transform keys to the on-GPU layout the
/// shader will read.
///
/// `awsm-renderer::Textures` implements this trait.
pub trait TextureContext {
    /// Resolves a NEUTRAL's pool placement: `(array, entry)` for the shared
    /// 1×1 white / flat-normal entries the renderer registers at boot, plus
    /// the default sampler's shader-visible index. `None` only in contexts
    /// with no pool at all (tests); packers then fall back to the zero
    /// sentinel, which is fine anywhere no shader actually samples.
    fn neutral_texture(
        &self,
        kind: NeutralTexture,
    ) -> Option<(
        &TexturePoolArray<TextureKey>,
        &TexturePoolEntryInfo<TextureKey>,
        u32,
    )>;

    /// Returns the array slot for a pooled texture, if it exists.
    fn pool_array_by_index(&self, index: usize) -> Option<&TexturePoolArray<TextureKey>>;

    /// Returns the per-texture entry info, if the texture exists.
    fn texture_entry(&self, key: TextureKey) -> Option<&TexturePoolEntryInfo<TextureKey>>;

    /// Returns the shader-visible sampler index for a sampler key, if the
    /// sampler is registered in the pool's sampler set.
    fn sampler_index(&self, key: SamplerKey) -> Option<u32>;

    /// Returns the U / V address modes for a sampler.
    ///
    /// Returns `(None, None)` if the sampler is unknown — packers should
    /// treat that as the default (Repeat).
    fn sampler_address_modes(&self, key: SamplerKey) -> (Option<AddressMode>, Option<AddressMode>);

    /// Returns the byte offset of a texture transform in the renderer's
    /// transform buffer, if the key is known.
    fn texture_transform_offset(&self, key: TextureTransformKey) -> Option<usize>;

    /// Returns the byte offset of the identity texture transform.
    fn texture_transform_identity_offset(&self) -> usize;
}
