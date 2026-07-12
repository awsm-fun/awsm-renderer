//! Bloom shader templates.

use askama::Template;

use crate::{
    render_passes::bloom::shader::cache_key::{
        BloomPyramidStep, ShaderCacheKeyBloomCombine, ShaderCacheKeyBloomDownsample,
    },
    shaders::{AwsmShaderError, Result},
};

/// Down-sample step template — the `prefilter` flag toggles the
/// composite-read + soft-knee threshold variant vs. the plain 13-tap pyramid
/// downsample.
#[derive(Template, Debug)]
#[template(path = "bloom_wgsl/downsample.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateBloomDownsampleStep {
    pub prefilter: bool,
}

/// Up-sample step template — progressive 9-tap tent accumulation into the
/// ping-pong up-pyramid (`up[N-1] = down[N-1] + scatter · tent9(mip N)`).
#[derive(Template, Debug, Default)]
#[template(path = "bloom_wgsl/upsample.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateBloomUpsample;

/// Bloom pyramid-step shader (prefilter / downsample / upsample). Named
/// `..BloomDownsample` because it is what the shared
/// `ShaderCacheKeyRenderPass::BloomDownsample` dispatch variant renders — the
/// upsample rides the same variant (see
/// [`ShaderCacheKeyBloomDownsample::step`]) so the cross-pass enums stay
/// bloom-agnostic.
pub enum ShaderTemplateBloomDownsample {
    Down(ShaderTemplateBloomDownsampleStep),
    Up(ShaderTemplateBloomUpsample),
}

impl TryFrom<&ShaderCacheKeyBloomDownsample> for ShaderTemplateBloomDownsample {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyBloomDownsample) -> Result<Self> {
        Ok(match value.step {
            BloomPyramidStep::Prefilter => {
                Self::Down(ShaderTemplateBloomDownsampleStep { prefilter: true })
            }
            BloomPyramidStep::Downsample => {
                Self::Down(ShaderTemplateBloomDownsampleStep { prefilter: false })
            }
            BloomPyramidStep::Upsample => Self::Up(ShaderTemplateBloomUpsample),
        })
    }
}

impl ShaderTemplateBloomDownsample {
    pub fn into_source(self) -> Result<String> {
        match self {
            Self::Down(tmpl) => tmpl.render().map_err(AwsmShaderError::from),
            Self::Up(tmpl) => tmpl.render().map_err(AwsmShaderError::from),
        }
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some(match self {
            Self::Down(tmpl) => {
                if tmpl.prefilter {
                    "Bloom Prefilter"
                } else {
                    "Bloom Downsample"
                }
            }
            Self::Up(_) => "Bloom Upsample",
        })
    }
}

/// Combine shader — tent-tap of the accumulated up-pyramid mip 0 into the
/// full-res bloom target.
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
