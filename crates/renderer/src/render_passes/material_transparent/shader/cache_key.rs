//! Shader cache key for the transparent material pass.

use awsm_materials::MaterialShaderId;

use crate::{
    dynamic_materials::ShadingBase,
    render_passes::{
        material_opaque::shader::cache_key::DynamicShaderInfo,
        shader_cache_key::ShaderCacheKeyRenderPass,
        shared::material::cache_key::ShaderMaterialVertexAttributes,
    },
    shaders::ShaderCacheKey,
};

/// Cache key for transparent material shaders.
///
/// Same shape as the pre-dynamic-materials build for the common path
/// (one fragment shader handles every transparent mesh with a runtime
/// branch on `shader_id`). When a dynamic material is registered,
/// `dynamic_shader` carries the auto-generated `MaterialData` struct
/// and the author's WGSL fragment, so the transparent fragment
/// template emits a wrapped `custom_shade_transparent_dynamic(...)`
/// function + dispatch arm — same model as the opaque cache key.
///
/// `dispatch_hash` mirrors the opaque variant's — `0` is the stable
/// empty-state sentinel that preserves bit-identical compiled WGSL
/// when no dynamic transparent materials are registered.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyMaterialTransparent {
    pub instancing_transforms: bool,
    pub attributes: ShaderMaterialVertexAttributes,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
    pub msaa_sample_count: Option<u32>,
    pub mipmaps: bool,
    /// Which built-in shading family this transparent material is — the
    /// fragment selects its body at COMPILE time (`{% if base == … %}`),
    /// not via a runtime `shader_id ==` branch. Each distinct
    /// `(base, pbr_features)` is its own specialized transparent pipeline
    /// (transparent is drawn per-mesh / one material per draw, so it's
    /// already material-homogeneous — no uber fragment, per the
    /// specialize-only pivot). `Custom` for a dynamic author material.
    pub base: ShadingBase,
    /// PBR feature mask ([`awsm_materials::pbr::PbrFeatures::bits`]) this
    /// transparent PBR pipeline is specialized for — drives the compile-
    /// time `{% if pbr_features.<x> %}` gating in the shared brdf /
    /// material_color_calc includes. Inert for non-PBR bases.
    pub pbr_features: u32,
    /// Stable hash over the currently-registered dynamic-material set
    /// (sorted by shader_id). `0` when none registered — pre-feature
    /// WGSL is bit-identical.
    pub dispatch_hash: u64,
    /// Per-mesh dynamic-material shader_id, if any. `Some` when the
    /// transparent mesh's material is `Material::Custom`; the
    /// fragment template emits the wrapper + dispatch arm for it.
    pub dynamic_shader_id: Option<MaterialShaderId>,
    /// Carries the registered material's struct decl + WGSL fragment
    /// when `dynamic_shader_id.is_some()`. The fragment template
    /// renders it into a `custom_shade_transparent_dynamic` function.
    pub dynamic_shader: Option<DynamicShaderInfo>,
    /// GPU light-culling froxel slice count baked into the consumer
    /// shader (matches the cull pass's WGSL `SLICE_COUNT`). The
    /// shading-time froxel index calculation constant-folds this.
    pub froxel_slice_count: u32,
}

impl From<ShaderCacheKeyMaterialTransparent> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyMaterialTransparent) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::MaterialTransparent(key))
    }
}
