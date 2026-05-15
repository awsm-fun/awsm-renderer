//! The `MaterialShader` trait — the public ABI of every shading model.
//!
//! See `README.md` for the contract overview.

use crate::{alpha_mode::MaterialAlphaMode, shader_id::MaterialShaderId, TextureContext};

/// A declarative description of a texture slot a material expects to bind.
///
/// The renderer takes the union of declared slots across enabled materials
/// when building the material pass's bind-group layout. Bindings are
/// addressed by `slot_name` in WGSL.
#[derive(Debug, Clone, Copy)]
pub struct TextureSlotDecl {
    /// Stable identifier for the slot — used as the WGSL binding name and as
    /// the lookup key on the renderer side. Must be unique within a material.
    pub slot_name: &'static str,
    /// Whether the material can ship without the slot bound (the writer
    /// falls back to `SkipTexture`).
    pub optional: bool,
}

/// Material shading contract.
///
/// Each material in this crate (gated by a Cargo feature) implements this
/// trait. The renderer walks the enabled set as a registry to:
///
/// - Concatenate `wgsl_fragment()` outputs into the `{{ materials_wgsl }}`
///   askama variable.
/// - Generate the `if shader_id == X { ... }` dispatch table as the
///   `{{ shader_id_dispatch }}` askama variable.
/// - Build the union of declared texture slots for the material pass's
///   bind-group layout.
/// - Dispatch `write_uniform_buffer` per material instance when packing the
///   material storage buffer.
pub trait MaterialShader {
    /// Stable per-build shader id. Written as the first u32 of the material's
    /// uniform buffer payload.
    fn shader_id(&self) -> MaterialShaderId;

    /// WGSL helper module for this material. The renderer concatenates every
    /// enabled material's fragment and feeds the result to the shader
    /// template as `{{ materials_wgsl }}`.
    ///
    /// The fragment must declare:
    /// - A `*_get_material(byte_offset: u32) -> StructType` accessor that
    ///   un-packs the material from the storage buffer.
    /// - A `compute_*_color(...)` / `compute_*_lit_color(...)` function the
    ///   dispatch table calls — see the existing PBR / Unlit / Toon
    ///   modules for the signatures the surrounding shader expects.
    fn wgsl_fragment(&self) -> &'static str;

    /// Material alpha mode (opaque / mask / blend).
    fn alpha_mode(&self) -> MaterialAlphaMode;

    /// Returns true if the material renders in the transparency pass.
    ///
    /// Typically `alpha_mode == Blend || alpha_mode == Mask` but materials
    /// with non-trivial extensions (PBR transmission, refraction, etc.) may
    /// also route to the transparency pass even when their alpha is opaque.
    fn is_transparency_pass(&self) -> bool;

    /// Pack this material's authored parameters into `out`.
    ///
    /// The first u32 written must be `self.shader_id().as_u32()`. The rest
    /// of the layout is private to the material and must match its
    /// `wgsl_fragment`'s accessor.
    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, out: &mut Vec<u8>);

    /// Texture slots this material declares.
    fn texture_slots(&self) -> &'static [TextureSlotDecl];
}
