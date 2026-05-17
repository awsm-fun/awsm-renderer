//! Abstraction over the renderer's texture pool / sampler set for material
//! uniform-buffer writers.
//!
//! Materials need a handful of texture/sampler lookups when packing their
//! payload — they don't need the full `Textures` struct from `awsm-renderer`.
//! Routing those lookups through this trait keeps `awsm-materials` from
//! depending on `awsm-renderer`.

use awsm_renderer_core::{
    keys::{SamplerKey, TextureKey, TextureTransformKey},
    sampler::AddressMode,
    texture::texture_pool::{TexturePoolArray, TexturePoolEntryInfo},
};

/// Renderer-side context that lets a material's `write_uniform_buffer` resolve
/// pooled texture / sampler / texture-transform keys to the on-GPU layout the
/// shader will read.
///
/// `awsm-renderer::Textures` implements this trait.
pub trait TextureContext {
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
