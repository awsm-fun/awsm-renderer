//! SSR shader templates.
//!
//! The askama `{% if %}` flags below are how SSR stays granular + zero-cost
//! (§5a): each structural axis is a template flag, so `Mirror` compiles without
//! the glossy sampling/denoise code, non-temporal compiles without the
//! reproject code, etc. The trace itself is always the linear-DDA march (the
//! Hi-Z accelerator was deleted; `SsrTrace::PRODUCTION` is `LinearDda`).

use askama::Template;

use crate::{
    render_passes::ssr::shader::cache_key::{ShaderCacheKeySsr, SsrMode},
    shaders::{AwsmShaderError, Result},
};

/// SSR trace compute shader.
#[derive(Template, Debug)]
#[template(path = "ssr_wgsl/trace.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSsr {
    /// GGX importance-sampled glossy path vs. the tight mirror ray.
    pub glossy: bool,
    /// Temporal reproject + neighbourhood clamp.
    pub temporal: bool,
    /// Half-res trace + guided upsample.
    pub half_res: bool,
    /// Multisampled depth + normal G-buffer bindings (MSAA).
    pub multisampled_geometry: bool,
    /// Depth convention (003).
    pub reverse_z: bool,
}

impl TryFrom<&ShaderCacheKeySsr> for ShaderTemplateSsr {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeySsr) -> Result<Self> {
        Ok(Self {
            glossy: value.mode == SsrMode::Glossy,
            temporal: value.temporal,
            half_res: value.half_res,
            multisampled_geometry: value.multisampled_geometry,
            reverse_z: value.reverse_z,
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
