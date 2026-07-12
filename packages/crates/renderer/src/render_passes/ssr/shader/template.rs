//! SSR shader templates.
//!
//! The askama `{% if %}` flags below are how SSR stays granular + zero-cost
//! (§5a): each structural axis is a template flag, so `Mirror` compiles without
//! the glossy sampling/denoise code, the non-temporal configuration compiles
//! no temporal module at all, etc.
//!
//! Three modules share the `ShaderTemplateSsr` slot: the trace
//! (`ssr_wgsl/trace.wgsl`), the spatial resolve (`ssr_wgsl/resolve.wgsl`) —
//! the edge-aware denoise that runs between trace and composite — and the
//! temporal accumulation (`ssr_wgsl/temporal.wgsl`) — the history reproject +
//! neighborhood clamp that runs after the resolve.

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
    Temporal(ShaderTemplateSsrTemporal),
}

/// SSR trace compute shader.
#[derive(Template, Debug)]
#[template(path = "ssr_wgsl/trace.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSsrTrace {
    /// GGX importance-sampled glossy path vs. the tight mirror ray.
    pub glossy: bool,
    /// Hi-Z traversal over the HZB closest-depth channel (vs linear DDA).
    pub hzb: bool,
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

/// SSR temporal-accumulation compute shader — history reproject +
/// neighborhood clamp after the spatial resolve (compiled only when
/// `post_processing.ssr.temporal`).
#[derive(Template, Debug)]
#[template(path = "ssr_wgsl/temporal.wgsl", whitespace = "minimize")]
pub struct ShaderTemplateSsrTemporal {
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
                half_res: key.half_res,
                multisampled_geometry: key.multisampled_geometry,
                reverse_z: key.reverse_z,
            }),
            ShaderCacheKeySsr::Resolve(key) => Self::Resolve(ShaderTemplateSsrResolve {
                multisampled_geometry: key.multisampled_geometry,
                reverse_z: key.reverse_z,
            }),
            ShaderCacheKeySsr::Temporal(key) => Self::Temporal(ShaderTemplateSsrTemporal {
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
            Self::Temporal(tmpl) => tmpl.render().map_err(AwsmShaderError::from),
        }
    }

    pub fn debug_label(&self) -> Option<&str> {
        match self {
            Self::Trace(_) => Some("SSR Trace"),
            Self::Resolve(_) => Some("SSR Resolve"),
            Self::Temporal(_) => Some("SSR Temporal"),
        }
    }
}
