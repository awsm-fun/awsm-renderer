//! Shader cache key for the **custom-vertex** shadow caster — the displaced-
//! shadow path that keeps a custom-vertex material's shadow glued to its lit
//! geometry (no detached / smooth shadow).
//!
//! A material that declared a `wgsl_vertex` displacement body gets its own
//! shadow-generation pipeline: the (depth-only) shadow VERTEX shader compiles
//! the gated `custom_displace_vertex` hook with the SAME inputs the geometry
//! custom-vertex pass uses, so the silhouette matches exactly. Because the hook's
//! `material_data_load` reads the renderer's `materials` storage buffer (and the
//! generated texture helpers sample the texture pool), the variant reuses the
//! **masked-shadow** bind group — augmented to give those bindings VERTEX
//! visibility (mirrors how the geometry custom-vertex variant reuses the masked
//! geometry bind group). The plain shadow bind groups declare none of that.
//!
//! Specialized per `shader_id` (like the masked-shadow variant) because the
//! assembled module embeds the material's auto-generated `MaterialData`
//! struct/loader + the author's vertex body — an edit recompiles only the
//! affected pipeline. The shadow atlas is single-sampled, so there is no MSAA /
//! sample_mask field.

use awsm_materials::MaterialShaderId;

use crate::{
    render_passes::geometry::shader::cache_key::DynamicVertexShaderInfo, shaders::ShaderCacheKey,
};

/// Cache key for the custom-vertex shadow-generation shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyShadowCustomVertex {
    /// Which material this custom-vertex variant displaces for. A dynamic id
    /// whose registration carries a `wgsl_vertex` body.
    pub shader_id: MaterialShaderId,
    /// The auto-generated `MaterialData` struct + loader plus the author's
    /// vertex-displacement WGSL body, wrapped into `custom_displace_vertex` at
    /// render time. The SAME `DynamicVertexShaderInfo` the geometry custom-vertex
    /// variant uses, so the displacement is byte-identical across the two passes.
    pub dynamic_vertex: DynamicVertexShaderInfo,
    /// Texture-pool array bindings the (reused masked-shadow) bind groups declare,
    /// so the hook's generated `material_sample_<name>` helpers can sample. Must
    /// match the live pool — growing it recompiles, exactly like the masked /
    /// custom-vertex geometry passes.
    pub texture_pool_arrays_len: u32,
    /// Texture-pool comparison/filter sampler bindings, same role.
    pub texture_pool_samplers_len: u32,
    /// Whether the pipeline reads the per-instance transform vertex buffer at
    /// slot 1 (and the uniform-with-dynamic-offset meta binding). The shadow
    /// custom-vertex path is non-instanced today (always `false`); the field is
    /// carried for parity with the plain + masked shadow variants.
    pub instancing_transforms: bool,
}

impl From<ShaderCacheKeyShadowCustomVertex> for ShaderCacheKey {
    fn from(value: ShaderCacheKeyShadowCustomVertex) -> Self {
        ShaderCacheKey::ShadowCustomVertex(value)
    }
}
