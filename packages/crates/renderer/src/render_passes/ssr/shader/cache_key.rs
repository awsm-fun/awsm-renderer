//! SSR shader cache keys.
//!
//! The cache key is the permutation IDENTITY: every
//! field that changes the compiled WGSL text lives here, so each distinct
//! variant compiles once and only the variants actually enabled are compiled.
//! The scalar tuning knobs (`intensity`, `thickness`, …) are LIVE uniforms and
//! deliberately DO NOT appear here — tweaking them never recompiles.

use crate::{render_passes::shader_cache_key::ShaderCacheKeyRenderPass, shaders::ShaderCacheKey};

/// Reflection lobe model. `Mirror` compiles a tight single-ray path (no
/// spread / importance-sampling / denoise code); `Glossy` compiles the
/// GGX-importance-sampled + descriptor-fetch + denoise path.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsrMode {
    Mirror,
    Glossy,
}

/// Ray-march strategy. `LinearDda` (verified pixel-perfect) is the only
/// strategy: the dormant Hi-Z min-Z-pyramid accelerator was deleted (Plan 004
/// Part 2) — as a pure accelerator it produced the same image as the DDA when
/// correct, its coarse-mip traversal banded, and DDA is sufficient for target
/// scenes.
#[derive(Hash, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsrTrace {
    LinearDda,
}

impl SsrTrace {
    /// The trace strategy compiled into the PRODUCTION SSR pipeline.
    pub const PRODUCTION: SsrTrace = SsrTrace::LinearDda;
}

/// Cache key for the SSR trace shader.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeySsr {
    pub mode: SsrMode,
    pub trace: SsrTrace,
    /// Temporal reproject + neighbourhood-clamp code exists only when true.
    pub temporal: bool,
    /// Half-res trace → the guided-upsample variant.
    pub half_res: bool,
    /// Under MSAA the depth + `normal_tangent` G-buffer targets are
    /// multisampled, so the binding types + `textureLoad` change. (The HDR
    /// color source stays the resolved single-sample `transparent` target.)
    pub multisampled_geometry: bool,
    /// Depth convention (003).
    pub reverse_z: bool,
}

impl From<ShaderCacheKeySsr> for ShaderCacheKey {
    fn from(key: ShaderCacheKeySsr) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Ssr(key))
    }
}
