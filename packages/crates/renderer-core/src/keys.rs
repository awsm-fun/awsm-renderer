//! Public slotmap key types shared across renderer crates.
//!
//! Lives in `awsm-renderer-core` so the upcoming `awsm-renderer-materials` crate (and any
//! other content-generation sibling) can reference textures + samplers without
//! depending on `awsm-renderer`. The slotmaps themselves still live in
//! `awsm-renderer::textures` — only the opaque key types are factored out.

use slotmap::new_key_type;

new_key_type! {
    /// Opaque key for pooled textures.
    pub struct TextureKey;
}

new_key_type! {
    /// Opaque key for texture transforms.
    pub struct TextureTransformKey;
}

new_key_type! {
    /// Opaque key for samplers.
    pub struct SamplerKey;
}
