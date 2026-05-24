//! Line shader template.

use askama::Template;

use crate::{
    render_passes::lines::shader::cache_key::ShaderCacheKeyLine,
    shaders::{AwsmShaderError, Result},
};

/// Static fat-line shader — no per-variant parameters; the
/// `depth_compare` / MSAA branching happens at pipeline-state
/// creation, not in the WGSL itself.
#[derive(Template, Debug, Default)]
#[template(path = "line_wgsl/line.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateLine;

impl TryFrom<&ShaderCacheKeyLine> for ShaderTemplateLine {
    type Error = AwsmShaderError;

    fn try_from(_value: &ShaderCacheKeyLine) -> Result<Self> {
        Ok(Self)
    }
}

impl ShaderTemplateLine {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    #[cfg(debug_assertions)]
    pub fn debug_label(&self) -> Option<&str> {
        Some("Line")
    }
}
