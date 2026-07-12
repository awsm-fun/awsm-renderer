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
/// trace, the spatial resolve, and the temporal accumulation are distinct
/// WGSL modules with distinct permutation axes, but they share the one
/// `ShaderCacheKey::Ssr` slot in the cross-renderer cache.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub enum ShaderCacheKeySsr {
    Trace(ShaderCacheKeySsrTrace),
    Resolve(ShaderCacheKeySsrResolve),
    Temporal(ShaderCacheKeySsrTemporal),
}

/// Cache key for the SSR trace shader (`ssr_wgsl/trace.wgsl`). Temporal
/// accumulation is NOT a trace axis: the history reproject + neighborhood
/// clamp live in the dedicated temporal pass (`ShaderCacheKeySsrTemporal`),
/// and the trace's per-frame jitter rotation is a RUNTIME gate on
/// `params.temporal_weight` (a uniform read, never a recompile).
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeySsrTrace {
    pub mode: SsrMode,
    pub trace: SsrTrace,
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

/// Cache key for the SSR temporal-accumulation shader
/// (`ssr_wgsl/temporal.wgsl`) — the history reproject + neighborhood-clamp
/// pass that runs AFTER the spatial resolve. Compiled only when
/// `post_processing.ssr.temporal`. Runs at the SSR target's own resolution
/// (it reads its output dims at runtime), so its only axes are the
/// depth-binding type + depth convention — same as the resolve.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeySsrTemporal {
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

impl From<ShaderCacheKeySsrTemporal> for ShaderCacheKey {
    fn from(key: ShaderCacheKeySsrTemporal) -> Self {
        ShaderCacheKeySsr::Temporal(key).into()
    }
}

/// Native replica of the trace's ADAPTIVE crossing acceptance
/// (`ssr_wgsl/trace.wgsl`, comment-pinned there; the exact WGSL term is also
/// pinned by `wgsl_validation::ssr_shaders_validate`):
///
/// ```wgsl
/// ray_z > scene_z + 1e-4 * scene_z
///     && (ray_z - scene_z) < max(params.thickness, 2.0 * step_advance)
/// ```
///
/// where `step_advance = |ray_z(s_cur) - ray_z(s_prev)|`. The per-step ray
/// depth advance bounds how deep a LEGITIMATE crossing can have penetrated
/// between two probes, so thin geometry is accepted regardless of subpixel
/// phase while travel far behind an object is still rejected. Keep in sync
/// with trace.wgsl (both march variants).
#[cfg(test)]
mod acceptance_tests {
    /// Mirrors the WGSL inequality exactly (see module doc above).
    fn accepts(ray_z_prev: f32, ray_z: f32, scene_z: f32, thickness: f32) -> bool {
        let step_advance = (ray_z - ray_z_prev).abs();
        ray_z > scene_z + 1e-4 * scene_z
            && (ray_z - scene_z) < f32::max(thickness, 2.0 * step_advance)
    }

    /// The thin-geometry lottery the adaptive term fixes: a coarse step
    /// crosses a thin tube with penetration far beyond the fixed thickness.
    /// The old test (`penetration < thickness`) rejected it — hit/miss then
    /// depended on subpixel phase, serrating the reflection. The adaptive
    /// bound (2x the step's own depth advance) accepts every such crossing.
    #[test]
    fn thin_crossing_accepted_regardless_of_subpixel_phase() {
        let thickness = 0.05;
        // scene_z = the thin tube's surface.
        let scene_z = 5.0;
        // Coarse step: the ray descends 1.5 world units across one probe and
        // lands 0.5 behind the tube's front surface.
        assert!(accepts(4.0, 5.5, scene_z, thickness));
        // Same crossing at a different subpixel phase (deeper penetration,
        // same step scale) must ALSO hit — no lottery.
        assert!(accepts(4.2, 5.9, scene_z, thickness));
        // Fine step near contact: penetration within the base thickness.
        assert!(accepts(4.99, 5.02, scene_z, thickness));
    }

    /// Travel far BEHIND an object must still be rejected: with fine steps
    /// (small depth advance) a ray 2.0 units past the surface is occluded
    /// territory, not a crossing — the adaptive bound stays near thickness.
    #[test]
    fn behind_object_travel_rejected() {
        let thickness = 0.05;
        let scene_z = 4.0;
        // Fine step (advance 0.1) deep behind the surface: reject.
        assert!(!accepts(5.9, 6.0, scene_z, thickness));
        // Even a moderately coarse step (advance 0.4 -> bound 0.8) does not
        // accept a 2.0-unit penetration.
        assert!(!accepts(5.6, 6.0, scene_z, thickness));
    }

    /// Exact contact (reflection meeting its reflector) is the case the old
    /// absolute `+ 0.01` front bias killed: penetration of ~1mm at 5m must
    /// be a hit now.
    #[test]
    fn exact_contact_accepted_without_absolute_bias() {
        let thickness = 0.05;
        let scene_z = 5.0;
        assert!(accepts(4.99, 5.001, scene_z, thickness));
    }

    /// Zero/negative penetration (ray still in front, or numerically ON the
    /// surface) stays a miss: the relative epsilon (1e-4 * scene_z) guards
    /// self-intersection without an absolute contact-killing bias.
    #[test]
    fn self_intersection_epsilon_rejects_zero_penetration() {
        let thickness = 0.05;
        let scene_z = 5.0;
        // Exactly on the surface: within the relative epsilon -> miss.
        assert!(!accepts(4.9, 5.0, scene_z, thickness));
        // A hair above the surface (in front): miss.
        assert!(!accepts(4.9, 4.9999, scene_z, thickness));
    }
}
