//! Shader cache key for the geometry pass.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Custom **vertex**-displacement shader info — the vertex-stage sibling of
/// [`DynamicShaderInfo`](crate::render_passes::material_opaque::shader::cache_key::DynamicShaderInfo).
///
/// Carried (as `Option`) by every rasterizing pass's cache key that can run a
/// custom-vertex variant (geometry / masked / transparent / shadow /
/// shadow-masked). `None` → the shared fast pipeline (zero cost for everyone
/// else); `Some` → that material gets its own pipeline whose `apply_vertex`
/// (or inline shadow chain) compiles the `custom_displace_vertex` hook.
///
/// The `struct_decl` / `loader_decl` are the SAME auto-generated `MaterialData`
/// declaration and loader the fragment hook uses (identical byte layout), so the
/// vertex and fragment stages read the same uniform buffer. Hashed alongside the
/// rest of the cache key so an edit recompiles only the affected pipelines.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct DynamicVertexShaderInfo {
    /// Author-declared shared-module set (already transitively resolved). The
    /// vertex stage wants a narrower set than the fragment (no lighting / IBL /
    /// shadows) — see `ShaderIncludes::for_vertex` (added with the pipelines).
    pub shader_includes: awsm_renderer_materials::ShaderIncludes,
    /// Auto-generated `struct MaterialData` decl (output of
    /// `dynamic_layout::generate_wgsl_struct`) — identical to the fragment hook's.
    pub struct_decl: String,
    /// Auto-generated `fn material_data_load(byte_offset: u32) -> MaterialData`
    /// accessor (output of `dynamic_layout::generate_wgsl_loader`).
    pub loader_decl: String,
    /// The author's WGSL displacement body, verbatim. Wrapped at template-render
    /// time into `fn custom_displace_vertex(input: VertexDisplaceInput) ->
    /// VertexDisplaceOutput { <body> }`.
    pub wgsl_vertex: String,
}

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
    /// `Some` → this is a per-material **custom-vertex** geometry pipeline: the
    /// vertex shader compiles the material's `custom_displace_vertex` hook (and
    /// the draw binds the material uniform/texture groups + a UV0 attribute).
    /// `None` → the shared fast geometry pipeline (zero cost for every mesh
    /// without a custom-vertex material). See [`DynamicVertexShaderInfo`].
    pub dynamic_vertex_shader: Option<DynamicVertexShaderInfo>,
}

impl From<ShaderCacheKeyGeometry> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyGeometry) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Geometry(key))
    }
}
