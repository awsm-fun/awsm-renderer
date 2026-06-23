//! Askama template for the shadow-generation vertex shader.

use askama::Template;

use crate::{
    shaders::{AwsmShaderError, Result},
    shadows::shader::cache_key::ShaderCacheKeyShadow,
};

/// Shadow generation shader template.
///
/// Renders the depth-only vertex shader; the pipeline has no fragment
/// stage. Locations 1..=4 on the vertex input are declared (the
/// visibility-geometry vertex buffer layout requires them) but unused
/// in the shadow pass.
#[derive(Template, Debug)]
#[template(path = "shadow_wgsl/vertex.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateShadow {
    pub instancing_transforms: bool,
    pub max_morph_unroll: u32,
    pub max_skin_unroll: u32,
    /// INERT scaffolding for the programmable vertex-displacement hook
    /// (mirrors the fragment `custom_shade_dynamic` machinery). Always
    /// `false` here — the gated WGSL is never rendered yet, so output is
    /// byte-identical to today.
    pub has_custom_vertex: bool,
    /// The author's WGSL displacement body, wrapped into
    /// `custom_displace_vertex` at render time. Empty until wired up.
    pub dynamic_wgsl_vertex: String,
    /// Auto-generated `struct MaterialData { ... }` decl for the hook.
    /// Empty until wired up.
    pub dynamic_vertex_struct_decl: String,
    /// Auto-generated `material_data_load` accessor for the hook.
    /// Empty until wired up.
    pub dynamic_vertex_loader_decl: String,
}

impl TryFrom<&ShaderCacheKeyShadow> for ShaderTemplateShadow {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyShadow) -> Result<Self> {
        Ok(Self {
            instancing_transforms: value.instancing_transforms,
            max_morph_unroll: 2,
            max_skin_unroll: 2,
            has_custom_vertex: false,
            dynamic_wgsl_vertex: String::new(),
            dynamic_vertex_struct_decl: String::new(),
            dynamic_vertex_loader_decl: String::new(),
        })
    }
}

impl ShaderTemplateShadow {
    /// Renders the template to WGSL source.
    pub fn into_source(self) -> Result<String> {
        Ok(self.render()?)
    }

    /// Optional debug label for shader compilation diagnostics.
    /// Kept in release builds too — labels are negligibly cheap and
    /// the WebGPU dev-tool / Spector.js surface they enable is worth
    /// it for the rare cases where someone's debugging a packaged
    /// build.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Shadow Generation VS")
    }
}
