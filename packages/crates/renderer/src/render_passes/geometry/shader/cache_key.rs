//! Shader cache key for the geometry pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Cache key for geometry pass shaders.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyGeometry {
    /// Variant takes per-instance vertex attributes (instance transform
    /// matrix). Controls vertex-buffer layout and the
    /// `apply_vertex` pre-skin transform path.
    pub instancing_transforms: bool,
    /// Variant reads `geometry_mesh_meta` from a `storage, read` array
    /// indexed by `@builtin(instance_index)` (true) versus from a
    /// `uniform` binding set per-draw via dynamic offset (false). True
    /// requires the WebGPU `indirect-first-instance` feature for
    /// drawIndirect to plumb the correct slot through; false is the
    /// portable shape that works on every device. Always false for
    /// the instanced path (which uses its own
    /// uniform-with-dynamic-offset binding regardless of the toggle).
    pub meta_storage_array: bool,
    pub msaa_samples: Option<u32>,
}

impl From<ShaderCacheKeyGeometry> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyGeometry) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Geometry(key))
    }
}
