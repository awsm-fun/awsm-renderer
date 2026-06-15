//! Shader cache key for the **masked** (alpha-tested) variant of the
//! geometry / visibility-buffer raster pass.
//!
//! A material with glTF `alphaMode = MASK` is alpha-tested *opaque* — it
//! belongs in the visibility/opaque path (so transmission samples it, it
//! casts shadows, and it's deferred-shaded), but its fragments below the
//! cutoff must be `discard`ed so cutouts are see-through. The plain
//! geometry raster writes depth unconditionally; this masked variant adds
//! the per-fragment cutoff `discard` before writing the visibility buffer,
//! so holes never write depth (and later depth-tested geometry / shadows /
//! transmission show through them).
//!
//! The variant is **specialized per `shader_id`** (mirroring the opaque
//! compute's `ShaderCacheKeyMaterialOpaque`) because the geometry template
//! cannot include the full `materials_wgsl` blob — it pulls dynamic-material
//! fragments that reference opaque-only contract types. Built-in materials
//! (PBR / Unlit / Toon) emit a minimal base-color-alpha load; a dynamic
//! (custom) material emits the author's *alpha-only* WGSL fragment.
//!
//! To stay within the `maxBindGroups = 4` ceiling the masked variant does
//! NOT add a 5th group: it reuses the plain geometry pass's groups 1
//! (transforms), 2 (uniform meta) and 3 (animation) verbatim, and *appends*
//! its fragment-only bindings (materials, material_mesh_metas, the merged
//! geometry pool, texture_transforms, texture pool) onto group 0 — which
//! already carries the camera/frame_globals uniforms the shared vertex
//! reads. The vertex path is therefore untouched.
//!
//! Masked meshes always take the non-instanced, uniform-meta, CPU-recorded
//! draw path (so this key carries no instancing / meta-storage toggles);
//! instanced masked meshes fall back to the plain (solid) pipeline.

use awsm_materials::MaterialShaderId;

use crate::{
    dynamic_materials::ShadingBase, render_passes::shader_cache_key::ShaderCacheKeyRenderPass,
    shaders::ShaderCacheKey,
};

/// Cache key for the masked geometry raster shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyGeometryMasked {
    /// Texture-pool array bindings the masked fragment declares (so a
    /// base-color / custom cutout can sample). Must match the live pool —
    /// growing the pool recompiles masked shaders, exactly like the opaque
    /// pass.
    pub texture_pool_arrays_len: u32,
    /// Texture-pool comparison/filter sampler bindings, same role.
    pub texture_pool_samplers_len: u32,
    /// MSAA sample count of the visibility buffer (e.g. `Some(4)`), or `None`
    /// for single-sampled. When multisampled, the masked fragment emits a
    /// `@builtin(sample_mask)` of analytic cutout coverage (instead of a binary
    /// `discard`) so the existing MSAA edge-resolve anti-aliases the cutout edge.
    pub msaa_samples: Option<u32>,
    /// Which material this masked variant alpha-tests for. Built-in ids
    /// (PBR/Unlit/Toon) take the base-color path; a dynamic id takes the
    /// custom alpha-only path via [`Self::dynamic_alpha`].
    pub shader_id: MaterialShaderId,
    /// Built-in shading family of the material — decoupled from `shader_id`
    /// so a per-feature-set PBR variant (dynamic-range id) still selects the
    /// base-color path.
    pub base: ShadingBase,
    /// `Some` when `shader_id.is_dynamic()`: the auto-generated `MaterialData`
    /// struct + loader, plus the author's *alpha-only* WGSL fragment (wrapped
    /// into `fn custom_alpha_dynamic(...) -> f32`). `None` for built-in ids.
    pub dynamic_alpha: Option<DynamicAlphaShaderInfo>,
}

/// Per-dynamic-material info embedded in the masked cache key so the
/// template can emit the author's alpha-only fragment + its `MaterialData`
/// accessor. Mirrors `material_opaque`'s `DynamicShaderInfo`, but carries
/// the *second* (alpha-only) WGSL window rather than the color fragment.
///
/// Hashed by the generated WGSL strings; two registrations that produce
/// byte-identical WGSL collapse to the same compiled shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct DynamicAlphaShaderInfo {
    /// The auto-generated `struct MaterialData { ... }` declaration
    /// (`dynamic_layout::generate_wgsl_struct`).
    pub struct_decl: String,
    /// The auto-generated `fn material_data_load(byte_offset) -> MaterialData`
    /// accessor (`dynamic_layout::generate_wgsl_loader`).
    pub loader_decl: String,
    /// The auto-generated per-texture `material_sample_<name>` helpers
    /// (`dynamic_layout::generate_wgsl_texture_helpers`) so a texture-based
    /// cutout can sample its bound textures.
    pub texture_helpers: String,
    /// The author's alpha-only WGSL fragment, verbatim. Wrapped at
    /// template-render time into
    /// `fn custom_alpha_dynamic(input: MaskAlphaInput) -> f32 { <fragment> }`.
    pub alpha_wgsl: String,
}

impl From<ShaderCacheKeyGeometryMasked> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyGeometryMasked) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::GeometryMasked(key))
    }
}
