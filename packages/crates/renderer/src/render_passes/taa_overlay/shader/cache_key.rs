//! Shader cache key for the TAA deforming-mesh velocity overlay (32 B/sample
//! fallback; see [`crate::render_passes::taa_overlay`]).

use awsm_renderer_materials::MaterialShaderId;

use crate::{
    render_passes::geometry::shader::cache_key::DynamicVertexShaderInfo,
    render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey,
};

/// The custom-vertex (vertex hook) half of an overlay variant: the same
/// inputs as the geometry pass's custom-vertex variant of that material.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct TaaOverlayCustomVertex {
    pub shader_id: MaterialShaderId,
    pub dynamic_vertex: DynamicVertexShaderInfo,
    pub texture_pool_arrays_len: u32,
    pub texture_pool_samplers_len: u32,
}

/// One overlay shader: the geometry vertex stage in its TAA (velocity)
/// variant, the overlay fragment.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyTaaOverlay {
    pub instancing_transforms: bool,
    /// Non-instanced meshes under `indirect-first-instance`: the mesh meta
    /// comes from the storage array (as in the geometry pass).
    pub meta_storage_array: bool,
    /// `Some` for a material with a vertex hook (non-instanced, uniform meta).
    pub custom_vertex: Option<TaaOverlayCustomVertex>,
    /// The debug check variant (`set_taa_debug_overlay_check`).
    pub debug: bool,
    /// Depth convention (only the debug variant reads it).
    pub reverse_z: bool,
}

impl From<ShaderCacheKeyTaaOverlay> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyTaaOverlay) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::TaaOverlay(key))
    }
}
