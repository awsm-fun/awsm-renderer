//! Texture reference used by materials.

use awsm_renderer_core::keys::{SamplerKey, TextureKey, TextureTransformKey};

/// A reference to a texture bound to one of a material's texture slots.
///
/// The fields mirror what the renderer needs to pack a `TextureInfo` into
/// the material's uniform buffer payload: which texture in the pool, which
/// sampler to use, which UV set to read, and optionally a UV transform.
#[derive(Clone, Debug)]
pub struct MaterialTexture {
    /// The pooled texture key.
    pub key: TextureKey,
    /// Sampler key. `None` means "use the default sampler for the slot."
    pub sampler_key: Option<SamplerKey>,
    /// Which UV set on the vertex (0 / 1 / …). `None` skips UV resolution.
    pub uv_index: Option<u32>,
    /// Optional UV transform (KHR_texture_transform-style).
    pub transform_key: Option<TextureTransformKey>,
}
