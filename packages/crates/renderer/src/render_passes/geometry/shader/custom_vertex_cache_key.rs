//! Shader cache key for the **custom-vertex** geometry raster variant.
//!
//! A material that declared a `wgsl_vertex` displacement body gets its own
//! geometry pipeline: the geometry VERTEX shader compiles the gated
//! `custom_displace_vertex` hook, while the fragment stays the PLAIN geometry
//! fragment (writes the visibility buffer; this variant is opaque, not
//! alpha-tested). Because the hook's `material_data_load` reads the renderer's
//! `materials` storage buffer (and the generated texture helpers sample the
//! texture pool), the variant reuses the **masked** geometry bind groups —
//! they declare those bindings on the augmented group 0, whereas the plain
//! geometry bind groups do not.
//!
//! Specialized per `shader_id` (like the masked variant) because the assembled
//! module embeds the material's auto-generated `MaterialData` struct/loader +
//! the author's vertex body — an edit recompiles only the affected pipeline.

use awsm_renderer_materials::MaterialShaderId;

use crate::{
    render_passes::geometry::shader::cache_key::DynamicVertexShaderInfo,
    render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey,
};

/// Cache key for the custom-vertex geometry raster shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyGeometryCustomVertex {
    /// Which material this custom-vertex variant displaces for. A dynamic id
    /// whose registration carries a `wgsl_vertex` body.
    pub shader_id: MaterialShaderId,
    /// The auto-generated `MaterialData` struct + loader plus the author's
    /// vertex-displacement WGSL body, wrapped into `custom_displace_vertex` at
    /// render time. Identical byte layout to the fragment hook's `MaterialData`.
    pub dynamic_vertex: DynamicVertexShaderInfo,
    /// Texture-pool array bindings the (reused masked) bind groups declare, so
    /// the hook's generated `material_sample_<name>` helpers can sample. Must
    /// match the live pool — growing it recompiles, exactly like the masked /
    /// opaque passes.
    pub texture_pool_arrays_len: u32,
    /// Texture-pool comparison/filter sampler bindings, same role.
    pub texture_pool_samplers_len: u32,
    /// MSAA sample count of the visibility buffer (e.g. `Some(4)`), or `None`
    /// for single-sampled. The plain geometry fragment is MSAA-agnostic; the
    /// field is carried so a multisampled pipeline re-keys distinctly.
    pub msaa_samples: Option<u32>,
    /// Variant takes per-instance vertex attributes (instance transform
    /// matrix). Mirrors [`ShaderCacheKeyGeometry::instancing_transforms`].
    pub instancing_transforms: bool,
    /// Variant reads `geometry_mesh_meta` from a `storage, read` array indexed
    /// by `@builtin(instance_index)` (true) vs. a `uniform` with dynamic offset
    /// (false). Mirrors [`ShaderCacheKeyGeometry::meta_storage_array`].
    ///
    /// Note: the reused masked bind groups declare the *uniform* meta binding,
    /// so the assembled module is only consistent with `false`; the field is
    /// kept for parity + future expansion.
    pub meta_storage_array: bool,
}

impl From<ShaderCacheKeyGeometryCustomVertex> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyGeometryCustomVertex) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::GeometryCustomVertex(key))
    }
}
