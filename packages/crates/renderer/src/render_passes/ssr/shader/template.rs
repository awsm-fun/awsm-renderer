//! SSR shader templates.
//!
//! The askama `{% if %}` flags below are how SSR stays granular + zero-cost
//! (§5a): each structural axis is a template flag, so `Mirror` compiles without
//! the glossy sampling/denoise code, non-temporal compiles without the
//! reproject code, etc. The trace itself is always the linear-DDA march (the
//! Hi-Z accelerator was deleted; `SsrTrace::PRODUCTION` is `LinearDda`).
//!
//! Two modules share the `ShaderTemplateSsr` slot: the trace
//! (`ssr_wgsl/trace.wgsl`) and the spatial resolve (`ssr_wgsl/resolve.wgsl`) —
//! the edge-aware denoise that runs between trace and composite.

use askama::Template;

use crate::{
    render_passes::ssr::shader::cache_key::{ShaderCacheKeySsr, SsrMode},
    shaders::{AwsmShaderError, Result},
};

/// SSR pass compute shaders — one variant per stage, dispatched from the one
/// `ShaderCacheKeySsr` cache slot.
#[derive(Debug)]
pub enum ShaderTemplateSsr {
    Trace(ShaderTemplateSsrTrace),
    Resolve(ShaderTemplateSsrResolve),
}

/// SSR trace compute shader.
#[derive(Template, Debug)]
#[template(path = "ssr_wgsl/trace.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSsrTrace {
    /// GGX importance-sampled glossy path vs. the tight mirror ray.
    pub glossy: bool,
    /// Hi-Z traversal over the HZB closest-depth channel (vs linear DDA).
    pub hzb: bool,
    /// Temporal reproject + neighbourhood clamp.
    pub temporal: bool,
    /// Half-res trace + guided upsample.
    pub half_res: bool,
    /// Multisampled depth + normal G-buffer bindings (MSAA).
    pub multisampled_geometry: bool,
    /// Depth convention (003).
    pub reverse_z: bool,
}

/// SSR spatial resolve compute shader — 9-tap edge-aware disk filter over the
/// raw trace output (denoise between trace and composite).
#[derive(Template, Debug)]
#[template(path = "ssr_wgsl/resolve.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSsrResolve {
    /// Multisampled depth binding (MSAA) — same axis as the trace.
    pub multisampled_geometry: bool,
    /// Depth convention (003).
    pub reverse_z: bool,
}

impl TryFrom<&ShaderCacheKeySsr> for ShaderTemplateSsr {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeySsr) -> Result<Self> {
        Ok(match value {
            ShaderCacheKeySsr::Trace(key) => Self::Trace(ShaderTemplateSsrTrace {
                glossy: key.mode == SsrMode::Glossy,
                hzb: key.trace == super::cache_key::SsrTrace::HiZ,
                temporal: key.temporal,
                half_res: key.half_res,
                multisampled_geometry: key.multisampled_geometry,
                reverse_z: key.reverse_z,
            }),
            ShaderCacheKeySsr::Resolve(key) => Self::Resolve(ShaderTemplateSsrResolve {
                multisampled_geometry: key.multisampled_geometry,
                reverse_z: key.reverse_z,
            }),
        })
    }
}

impl ShaderTemplateSsr {
    pub fn into_source(self) -> Result<String> {
        match self {
            Self::Trace(tmpl) => tmpl.render().map_err(AwsmShaderError::from),
            Self::Resolve(tmpl) => tmpl.render().map_err(AwsmShaderError::from),
        }
    }

    pub fn debug_label(&self) -> Option<&str> {
        match self {
            Self::Trace(_) => Some("SSR Trace"),
            Self::Resolve(_) => Some("SSR Resolve"),
        }
    }
}
