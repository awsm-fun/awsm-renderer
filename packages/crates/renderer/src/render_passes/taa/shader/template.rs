//! TAA resolve shader template. The structural axes are askama flags, so a
//! variant without the reactive mask contains no reactive binding or read.

use askama::Template;

use crate::{
    render_passes::taa::shader::cache_key::ShaderCacheKeyTaa,
    shaders::{AwsmShaderError, Result},
};

/// The TAA resolve compute shader.
#[derive(Template, Debug)]
#[template(path = "taa_wgsl/resolve.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateTaa {
    /// Depth convention (003).
    pub reverse_z: bool,
    /// Reactive-mask binding (plan phase 3).
    pub reactive: bool,
}

impl TryFrom<&ShaderCacheKeyTaa> for ShaderTemplateTaa {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyTaa) -> Result<Self> {
        Ok(Self {
            reverse_z: value.reverse_z,
            reactive: value.reactive,
        })
    }
}

impl ShaderTemplateTaa {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("TAA Resolve")
    }
}
