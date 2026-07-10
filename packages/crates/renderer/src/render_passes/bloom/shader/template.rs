//! Bloom shader templates.

use askama::Template;

use crate::{
    render_passes::bloom::shader::cache_key::{
        ShaderCacheKeyBloomCombine, ShaderCacheKeyBloomDownsample,
    },
    shaders::{AwsmShaderError, Result},
};

/// Down-sample shader — the `prefilter` flag toggles the composite-read +
/// soft-knee threshold variant vs. the plain 13-tap pyramid downsample.
#[derive(Template, Debug)]
#[template(path = "bloom_wgsl/downsample.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateBloomDownsample {
    pub prefilter: bool,
}

impl TryFrom<&ShaderCacheKeyBloomDownsample> for ShaderTemplateBloomDownsample {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyBloomDownsample) -> Result<Self> {
        Ok(Self {
            prefilter: value.prefilter,
        })
    }
}

impl ShaderTemplateBloomDownsample {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some(if self.prefilter {
            "Bloom Prefilter"
        } else {
            "Bloom Downsample"
        })
    }
}

/// Combine shader — mip-sum upsample of the pyramid into the full-res bloom.
#[derive(Template, Debug, Default)]
#[template(path = "bloom_wgsl/combine.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateBloomCombine;

impl TryFrom<&ShaderCacheKeyBloomCombine> for ShaderTemplateBloomCombine {
    type Error = AwsmShaderError;

    fn try_from(_value: &ShaderCacheKeyBloomCombine) -> Result<Self> {
        Ok(Self)
    }
}

impl ShaderTemplateBloomCombine {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("Bloom Combine")
    }
}
