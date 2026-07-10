//! HZB shader templates.

use askama::Template;

use crate::{
    render_passes::hzb::shader::cache_key::{ShaderCacheKeyHzbReduce, ShaderCacheKeyHzbSeed},
    shaders::{AwsmShaderError, Result},
};

/// Seed shader — copies the depth buffer into HZB mip 0 as r32float.
/// `multisampled_geometry` toggles the depth binding type.
#[derive(Template, Debug)]
#[template(path = "hzb_wgsl/seed.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateHzbSeed {
    pub multisampled_geometry: bool,
    pub reverse_z: bool,
}

impl TryFrom<&ShaderCacheKeyHzbSeed> for ShaderTemplateHzbSeed {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyHzbSeed) -> Result<Self> {
        Ok(Self {
            multisampled_geometry: value.msaa_sample_count.is_some(),
            reverse_z: value.reverse_z,
        })
    }
}

impl ShaderTemplateHzbSeed {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("HZB Seed")
    }
}

/// Reduce shader — reduces 2×2 of mip N-1 into mip N, keeping the FARTHEST
/// depth (max forward-Z, min reverse-Z).
#[derive(Template, Debug, Default)]
#[template(path = "hzb_wgsl/reduce.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateHzbReduce {
    pub reverse_z: bool,
}

impl TryFrom<&ShaderCacheKeyHzbReduce> for ShaderTemplateHzbReduce {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyHzbReduce) -> Result<Self> {
        Ok(Self {
            reverse_z: value.reverse_z,
        })
    }
}

impl ShaderTemplateHzbReduce {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("HZB Reduce")
    }
}
