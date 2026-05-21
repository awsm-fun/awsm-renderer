use askama::Template;

use crate::{
    render_passes::coverage::shader::cache_key::ShaderCacheKeyCoverage,
    shaders::{AwsmShaderError, Result},
};

#[derive(Template, Debug, Default)]
#[template(path = "coverage_wgsl/compute.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateCoverage {
    pub multisampled: bool,
}

impl TryFrom<&ShaderCacheKeyCoverage> for ShaderTemplateCoverage {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyCoverage) -> Result<Self> {
        Ok(Self {
            multisampled: value.multisampled,
        })
    }
}

impl ShaderTemplateCoverage {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    #[cfg(debug_assertions)]
    pub fn debug_label(&self) -> Option<&str> {
        Some("Coverage")
    }
}
