//! Shader cache key for the **masked** (alpha-tested) variant of the
//! shadow-generation pass — the B2 hole-shaped-shadow path.
//!
//! A glTF `alphaMode = MASK` caster must cast a *cutout* shadow: fragments
//! below the cutoff are `discard`ed so the hole doesn't write shadow depth and
//! later depth-tested receivers see light through it. The plain shadow pass is
//! depth-only with no fragment; this masked variant adds a fragment that
//! reconstructs the masking alpha (base-color × factor, or the custom material's
//! alpha-only WGSL) and discards below `MaterialMeshMeta.alpha_cutoff`.
//!
//! Specialized per `shader_id` for the same reason as the masked geometry
//! variant: the template can't pull the full `materials_wgsl` blob (opaque-only
//! contract types). Built-in PBR / Unlit / Toon share the base-color path; a
//! dynamic (custom) material emits the author's alpha-only fragment.
//!
//! Reuses [`DynamicAlphaShaderInfo`] from the masked geometry variant so a
//! custom material's alpha-only window is wrapped identically in both passes.
//! The cutout alpha logic itself lives in `shared_wgsl/masked_alpha.wgsl`,
//! included by both fragments.
//!
//! The shadow atlas is single-sampled, so there is no MSAA / `sample_mask`
//! field (binary discard; PCF/PCSS softens the edge). The variant DOES fork by
//! `instancing_transforms` (the shadow pass draws instanced casters), which
//! selects the storage-array vs uniform-with-dynamic-offset meta binding —
//! matching the plain shadow pipeline's two layouts.

use awsm_renderer_materials::MaterialShaderId;

use crate::{
    dynamic_materials::ShadingBase,
    render_passes::geometry::shader::masked_cache_key::DynamicAlphaShaderInfo,
    shaders::ShaderCacheKey,
};

/// Cache key for the masked shadow-generation shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyShadowMasked {
    /// Texture-pool array bindings the masked fragment declares (so a base-color
    /// / custom cutout can sample). Must match the live pool — growing the pool
    /// recompiles masked shaders, exactly like the geometry masked variant.
    pub texture_pool_arrays_len: u32,
    /// Texture-pool comparison/filter sampler bindings, same role.
    pub texture_pool_samplers_len: u32,
    /// Which material this masked variant alpha-tests for. Built-in ids
    /// (PBR/Unlit/Toon) take the base-color path; a dynamic id takes the custom
    /// alpha-only path via [`Self::dynamic_alpha`].
    pub shader_id: MaterialShaderId,
    /// Built-in shading family of the material — decoupled from `shader_id` so a
    /// per-feature-set PBR variant (dynamic-range id) still selects the
    /// base-color path.
    pub base: ShadingBase,
    /// `Some` when `shader_id.is_dynamic()`: the auto-generated `MaterialData`
    /// struct + loader plus the author's alpha-only WGSL. `None` for built-in.
    pub dynamic_alpha: Option<DynamicAlphaShaderInfo>,
    /// Whether the pipeline reads the per-instance transform vertex buffer at
    /// slot 1 (and the uniform-with-dynamic-offset meta binding). Must match the
    /// pipeline's vertex-buffer layout + meta bind-group layout.
    pub instancing_transforms: bool,
}

impl From<ShaderCacheKeyShadowMasked> for ShaderCacheKey {
    fn from(value: ShaderCacheKeyShadowMasked) -> Self {
        ShaderCacheKey::ShadowMasked(value)
    }
}
