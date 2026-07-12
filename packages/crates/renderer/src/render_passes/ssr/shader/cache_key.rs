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
    /// Per-pixel screen-space DDA — the fallback when no HZB exists
    /// (`features.gpu_culling` off, which is what allocates the pyramid).
    LinearDda,
    /// Hi-Z traversal over the HZB's closest-depth channel (.g): long rays
    /// skip whole empty cells at coarse mips instead of probing every texel,
    /// bounding iteration count logarithmically. Requires the dual-extreme
    /// HZB (built whenever SSR is enabled — see `optimization_policy`).
    HiZ,
}

/// Cache key for the SSR pass shaders — one variant per compute stage. The
/// trace and the spatial resolve are distinct WGSL modules with distinct
/// permutation axes, but they share the one `ShaderCacheKey::Ssr` slot in the
/// cross-renderer cache.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub enum ShaderCacheKeySsr {
    Trace(ShaderCacheKeySsrTrace),
    Resolve(ShaderCacheKeySsrResolve),
}

/// Cache key for the SSR trace shader (`ssr_wgsl/trace.wgsl`).
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeySsrTrace {
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

/// Cache key for the SSR spatial resolve shader (`ssr_wgsl/resolve.wgsl`) —
/// the edge-aware denoise between trace and composite. Runs at the SSR
/// target's own resolution regardless of `half_res` (it reads its output dims
/// at runtime), so its only axes are the depth-binding type + depth convention.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeySsrResolve {
    /// Under MSAA the full-res depth target is multisampled, so the depth
    /// binding's WGSL type changes — mirroring the trace's depth handling.
    pub multisampled_geometry: bool,
    /// Depth convention (003) — selects the sky early-out test.
    pub reverse_z: bool,
}

impl From<ShaderCacheKeySsr> for ShaderCacheKey {
    fn from(key: ShaderCacheKeySsr) -> Self {
        ShaderCacheKey::RenderPass(ShaderCacheKeyRenderPass::Ssr(key))
    }
}

impl From<ShaderCacheKeySsrTrace> for ShaderCacheKey {
    fn from(key: ShaderCacheKeySsrTrace) -> Self {
        ShaderCacheKeySsr::Trace(key).into()
    }
}

impl From<ShaderCacheKeySsrResolve> for ShaderCacheKey {
    fn from(key: ShaderCacheKeySsrResolve) -> Self {
        ShaderCacheKeySsr::Resolve(key).into()
    }
}
