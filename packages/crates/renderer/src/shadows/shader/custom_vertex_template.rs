//! Askama template for the **custom-vertex** shadow-generation shader.
//!
//! Depth-only (no fragment): the augmented custom-vertex shadow bind groups
//! (`shadow_custom_vertex_wgsl/bind_groups.wgsl` — shadow_view + materials +
//! frame_globals_raw + texture pool + the minimal material-load helpers) paired
//! with the custom-vertex shadow VERTEX shader (compiles the
//! `custom_displace_vertex` hook with the SAME inputs the geometry custom-vertex
//! pass uses). The bind-groups module is `{% include %}`d by the vertex module,
//! so this template renders ONE file.

use askama::Template;

use crate::{
    shaders::{AwsmShaderError, Result},
    shadows::shader::custom_vertex_cache_key::ShaderCacheKeyShadowCustomVertex,
};

/// Custom-vertex shadow shader template (single vertex module; depth-only).
#[derive(Template, Debug)]
#[template(
    path = "shadow_custom_vertex_wgsl/vertex.wgsl",
    whitespace = "minimize"
)]
pub struct ShaderTemplateShadowCustomVertex {
    /// Whether the pipeline reads the per-instance transform vertex buffer at
    /// slot 1 (and the uniform-with-dynamic-offset meta binding).
    instancing_transforms: bool,
    max_morph_unroll: u32,
    max_skin_unroll: u32,
    /// Texture-pool array bindings the included bind groups declare.
    texture_pool_arrays_len: u32,
    /// Texture-pool sampler bindings, same role.
    texture_pool_samplers_len: u32,
    /// Auto-generated `struct MaterialData { ... }` decl for the hook.
    dynamic_vertex_struct_decl: String,
    /// Auto-generated `material_data_load` accessor for the hook.
    dynamic_vertex_loader_decl: String,
    /// The author's WGSL displacement body, wrapped into `custom_displace_vertex`.
    dynamic_wgsl_vertex: String,
}

impl TryFrom<&ShaderCacheKeyShadowCustomVertex> for ShaderTemplateShadowCustomVertex {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyShadowCustomVertex) -> Result<Self> {
        Ok(Self {
            instancing_transforms: value.instancing_transforms,
            max_morph_unroll: 2,
            max_skin_unroll: 2,
            texture_pool_arrays_len: value.texture_pool_arrays_len,
            texture_pool_samplers_len: value.texture_pool_samplers_len,
            dynamic_vertex_struct_decl: value.dynamic_vertex.struct_decl.clone(),
            dynamic_vertex_loader_decl: value.dynamic_vertex.loader_decl.clone(),
            dynamic_wgsl_vertex: value.dynamic_vertex.wgsl_vertex.clone(),
        })
    }
}

impl ShaderTemplateShadowCustomVertex {
    /// Renders the custom-vertex shadow shader template into WGSL.
    pub fn into_source(self) -> Result<String> {
        Ok(self.render()?)
    }

    /// Optional debug label for shader compilation diagnostics.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Shadow Custom Vertex VS")
    }
}
