//! SSR min-Z pyramid shader templates.

use askama::Template;

use crate::{
    render_passes::ssr_minz::shader::cache_key::{
        ShaderCacheKeySsrMinzReduce, ShaderCacheKeySsrMinzSeed,
    },
    shaders::{AwsmShaderError, Result},
};

/// Seed shader — copies the depth buffer into min-Z pyramid mip 0 as
/// r32float. `multisampled_geometry` toggles the depth binding type (kept in
/// lockstep with the SSR trace's depth binding).
#[derive(Template, Debug)]
#[template(path = "ssr_minz_wgsl/seed.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSsrMinzSeed {
    pub multisampled_geometry: bool,
}

impl TryFrom<&ShaderCacheKeySsrMinzSeed> for ShaderTemplateSsrMinzSeed {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeySsrMinzSeed) -> Result<Self> {
        Ok(Self {
            multisampled_geometry: value.msaa_sample_count.is_some(),
        })
    }
}

impl ShaderTemplateSsrMinzSeed {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("SSR MinZ Seed")
    }
}

/// Reduce shader — min-reduces 2×2 of mip N-1 into mip N.
#[derive(Template, Debug, Default)]
#[template(path = "ssr_minz_wgsl/reduce.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSsrMinzReduce;

impl TryFrom<&ShaderCacheKeySsrMinzReduce> for ShaderTemplateSsrMinzReduce {
    type Error = AwsmShaderError;

    fn try_from(_value: &ShaderCacheKeySsrMinzReduce) -> Result<Self> {
        Ok(Self)
    }
}

impl ShaderTemplateSsrMinzReduce {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("SSR MinZ Reduce")
    }
}
