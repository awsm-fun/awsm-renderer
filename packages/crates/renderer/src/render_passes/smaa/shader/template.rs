//! SMAA shader templates (static sources — templating only for consistency
//! with the shader-cache plumbing).

use askama::Template;

use crate::{
    render_passes::smaa::shader::cache_key::{ShaderCacheKeySmaa, SmaaStep},
    shaders::{AwsmShaderError, Result},
};

#[derive(Template, Debug, Default)]
#[template(path = "smaa_wgsl/edges.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSmaaEdges;

#[derive(Template, Debug, Default)]
#[template(path = "smaa_wgsl/weights.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSmaaWeights;

pub enum ShaderTemplateSmaa {
    Edges(ShaderTemplateSmaaEdges),
    Weights(ShaderTemplateSmaaWeights),
}

impl TryFrom<&ShaderCacheKeySmaa> for ShaderTemplateSmaa {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeySmaa) -> Result<Self> {
        Ok(match value.step {
            SmaaStep::Edges => Self::Edges(ShaderTemplateSmaaEdges),
            SmaaStep::Weights => Self::Weights(ShaderTemplateSmaaWeights),
        })
    }
}

impl ShaderTemplateSmaa {
    pub fn into_source(self) -> Result<String> {
        match self {
            Self::Edges(tmpl) => tmpl.render().map_err(AwsmShaderError::from),
            Self::Weights(tmpl) => tmpl.render().map_err(AwsmShaderError::from),
        }
    }

    pub fn debug_label(&self) -> Option<&str> {
        match self {
            Self::Edges(_) => Some("smaa edges"),
            Self::Weights(_) => Some("smaa weights"),
        }
    }
}
