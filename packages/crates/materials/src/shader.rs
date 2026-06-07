//! The `MaterialShader` trait — the public ABI of every shading model.

use crate::{
    alpha_mode::MaterialAlphaMode,
    shader_id::MaterialShaderId,
    shader_includes::{FragmentInputs, ShaderIncludes},
    TextureContext,
};

/// Material shading contract.
///
/// Each material in this crate (gated by a Cargo feature) implements this
/// trait. The renderer walks the enabled set as a registry to:
///
/// - Emit `wgsl_fragment()` output into the `{{ materials_wgsl }}` askama
///   variable — filtered to the pipeline's own base in the specialized
///   opaque/transparent paths, unfiltered only in the no-geometry empty kernel.
/// - Generate the `if shader_id == X { ... }` dispatch table as the
///   `{{ shader_id_dispatch }}` askama variable.
/// - Dispatch `write_uniform_buffer` per material instance when packing the
///   material storage buffer.
pub trait MaterialShader {
    /// Stable per-build shader id. Written as the first u32 of the material's
    /// uniform buffer payload.
    fn shader_id(&self) -> MaterialShaderId;

    /// The shared shader modules this material's shading body uses. The renderer
    /// compiles the transitive closure (see [`ShaderIncludes::resolve`]) and
    /// emits only those `{% include %}`s — nothing is force-added. A material
    /// that returns [`ShaderIncludes::empty`] (e.g. a solid-color debug view)
    /// pulls no shared shading code at all.
    fn shader_includes(&self) -> ShaderIncludes;

    /// The pre-shade fragment inputs this material's shading body consumes. The
    /// pass scaffolding only unpacks/computes the declared ones (a material that
    /// returns [`FragmentInputs::empty`] skips TBN unpack, the lights read, …).
    fn fragment_inputs(&self) -> FragmentInputs;

    /// WGSL helper module for this material, fed to the shader template as
    /// `{{ materials_wgsl }}`. Each opaque/transparent pipeline is specialized
    /// to one `shader_id` + base, so the renderer typically emits only the
    /// matching base's fragment(s) — `build_materials_wgsl_filtered` — rather
    /// than the concat of every enabled material (the unfiltered
    /// `build_materials_wgsl` is used only by the no-geometry empty kernel).
    /// A fragment must therefore be self-contained for its own pipeline and
    /// must not assume any other material's fragment is present.
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
}
