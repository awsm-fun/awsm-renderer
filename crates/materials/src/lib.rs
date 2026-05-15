//! Pluggable material shaders for the `awsm-renderer` visibility-buffer pipeline.
//!
//! See `README.md` for a high-level overview, the trait contract, and the
//! rationale for the askama-substitution mechanism.
//!
//! Each material is a Cargo feature in this crate (`pbr-standard`, `unlit`,
//! `toon`). The `MaterialShader` trait is the contract every shading model
//! satisfies. `awsm-renderer` walks the enabled set as a registry to generate
//! the dispatch table and concatenate WGSL fragments at shader-template time.

pub mod alpha_mode;
pub mod registry;
pub mod shader;
pub mod shader_id;
pub mod texture;
pub mod texture_context;
pub mod writer;

#[cfg(feature = "pbr-standard")]
pub mod pbr;

#[cfg(feature = "unlit")]
pub mod unlit;

#[cfg(feature = "toon")]
pub mod toon;

pub use alpha_mode::MaterialAlphaMode;
pub use shader::{MaterialShader, TextureSlotDecl};
pub use shader_id::MaterialShaderId;
pub use texture::MaterialTexture;
pub use texture_context::TextureContext;
