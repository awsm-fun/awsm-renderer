//! Shader templates for the display pass.

use askama::Template;

use crate::{
    post_process::ToneMapping,
    render_passes::display::shader::cache_key::ShaderCacheKeyDisplay,
    shaders::{AwsmShaderError, Result},
};

/// Display pass shader template components.
#[derive(Debug)]
pub struct ShaderTemplateDisplay {
    pub bind_groups: ShaderTemplateDisplayBindGroups,
    pub vertex: ShaderTemplateDisplayVertex,
    pub fragment: ShaderTemplateDisplayFragment,
}

/// Bind group template for the display pass.
#[derive(Template, Debug)]
#[template(path = "display_wgsl/bind_groups.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateDisplayBindGroups {}

impl ShaderTemplateDisplayBindGroups {
    /// Creates a bind group template from the cache key.
    pub fn new(_cache_key: &ShaderCacheKeyDisplay) -> Self {
        Self {}
    }
}

/// Vertex shader template for the display pass.
#[derive(Template, Debug)]
#[template(path = "display_wgsl/vertex.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateDisplayVertex {}

impl ShaderTemplateDisplayVertex {
    /// Creates a vertex shader template from the cache key.
    pub fn new(_cache_key: &ShaderCacheKeyDisplay) -> Self {
        Self {}
    }
}

/// Fragment shader template for the display pass.
#[derive(Template, Debug)]
#[template(path = "display_wgsl/fragment.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateDisplayFragment {
    pub tonemapping: ToneMapping,
    pub supersample: bool,
}

impl ShaderTemplateDisplayFragment {
    /// Creates a fragment shader template from the cache key.
    pub fn new(cache_key: &ShaderCacheKeyDisplay) -> Self {
        Self {
            tonemapping: cache_key.tonemapping,
            supersample: cache_key.supersample,
        }
    }
}

impl TryFrom<&ShaderCacheKeyDisplay> for ShaderTemplateDisplay {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyDisplay) -> Result<Self> {
        Ok(Self {
            bind_groups: ShaderTemplateDisplayBindGroups::new(value),
            vertex: ShaderTemplateDisplayVertex::new(value),
            fragment: ShaderTemplateDisplayFragment::new(value),
        })
    }
}

impl ShaderTemplateDisplay {
    /// Renders the display shader template into WGSL.
    pub fn into_source(self) -> Result<String> {
        let bind_groups_source = self.bind_groups.render()?;
        let vertex_source = self.vertex.render()?;
        let fragment_source = self.fragment.render()?;
        Ok(format!(
            "{}\n{}\n{}",
            bind_groups_source, vertex_source, fragment_source
        ))
    }

    /// Returns an optional debug label for shader compilation.
    pub fn debug_label(&self) -> Option<&str> {
        Some("Display")
    }
}
