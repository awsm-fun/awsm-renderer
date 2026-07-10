//! SSR shader templates.
//!
//! The askama `{% if %}` flags below are how SSR stays granular + zero-cost
//! (§5a): each structural axis is a template flag, so `Mirror` compiles without
//! the glossy sampling/denoise code, non-temporal compiles without the
//! reproject code, etc. The M1 path is the else-branches (mirror / linear DDA /
//! non-temporal); the glossy / Hi-Z / temporal branches fill in at M2–M3.

use askama::Template;

use crate::{
    render_passes::ssr::shader::cache_key::{ShaderCacheKeySsr, SsrMode, SsrTrace},
    shaders::{AwsmShaderError, Result},
};

/// SSR trace compute shader.
#[derive(Template, Debug)]
#[template(path = "ssr_wgsl/trace.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSsr {
    /// GGX importance-sampled glossy path vs. the tight mirror ray.
    pub glossy: bool,
    /// Hi-Z (min-Z pyramid) march vs. the linear DDA march.
    pub hiz: bool,
    /// Temporal reproject + neighbourhood clamp.
    pub temporal: bool,
    /// Half-res trace + guided upsample.
    pub half_res: bool,
    /// Multisampled depth + normal G-buffer bindings (MSAA).
    pub multisampled_geometry: bool,
}

impl TryFrom<&ShaderCacheKeySsr> for ShaderTemplateSsr {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeySsr) -> Result<Self> {
        Ok(Self {
            glossy: value.mode == SsrMode::Glossy,
            hiz: value.trace == SsrTrace::HiZ,
            temporal: value.temporal,
            half_res: value.half_res,
            multisampled_geometry: value.multisampled_geometry,
        })
    }
}

impl ShaderTemplateSsr {
    pub fn into_source(self) -> Result<String> {
        self.render().map_err(AwsmShaderError::from)
    }

    pub fn debug_label(&self) -> Option<&str> {
        Some("SSR Trace")
    }
}
