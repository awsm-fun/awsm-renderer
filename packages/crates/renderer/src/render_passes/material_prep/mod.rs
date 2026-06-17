//! Material **prep** pass — the shared, material-independent per-pixel resolve
//! that Plan B introduces (`docs/plans/deferred-shared-prep-pass.md`).
//!
//! It runs once over the visibility buffer (after classify, before per-material
//! shading) and materializes everything that is the *same regardless of
//! material*: world position, interpolated UV sets + vertex colors, and the
//! per-pixel shadow-visibility terms. Per-material kernels then read those
//! buffers instead of recomputing them, so the per-material module shrinks.
//!
//! Stage 0 (this commit) lands only [`PrepPassConfig`] — the build-time knobs
//! every later stage keys on. The pass, its buffers, and bind groups arrive in
//! the subsequent stages (see the spec's "Implementation stages").

/// Build-time configuration for the shared prep + deferred-shadow path.
///
/// Stored on the renderer at construction (mirrors `AntiAliasing` /
/// `BucketConfig`). Inert until the prep pass is wired in (Stage 1+).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrepPassConfig {
    /// A/B flag. `false` (default) keeps the legacy path where each per-material
    /// kernel reconstructs attributes + samples shadows inline. `true` routes
    /// through the shared prep pass + slim per-material shading. Kept until the
    /// new path is proven on by default (Stage 6).
    pub enabled: bool,

    /// `K` — the maximum shadow casters that can overlap a *single pixel* (NOT
    /// total scene casters). Sizes the per-pixel shadow-visibility buffer to `K`
    /// layers; the j-th shadowed light in a pixel's froxel, in froxel-list
    /// order, writes layer `j`. Overflow (>K shadowed lights over one pixel) is
    /// clamped + logged. Default 4.
    pub max_shadow_casters_per_pixel: u32,

    /// World-position tunable. `false` (default) **materializes** world position
    /// in the prep pass (fp32, via the existing perspective-correct vertex
    /// interpolation — NOT depth unprojection). `true` falls back to
    /// reconstructing it in the slim shader (keeps `positions.wgsl` in the
    /// material module but saves the world-position buffer's bandwidth — the
    /// main 4K cost). The default is chosen from the Stage-6 measurement sweep.
    pub reconstruct_world_pos: bool,
}

impl Default for PrepPassConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_shadow_casters_per_pixel: 4,
            reconstruct_world_pos: false,
        }
    }
}

impl PrepPassConfig {
    /// Hard ceiling for `K` (per-pixel shadow-caster layers). Keeps the
    /// shadow-visibility buffer's VRAM/bandwidth bounded; values above this are
    /// clamped at build time. 16 layers @4K ≈ 133 MB R8 — already generous.
    pub const MAX_SHADOW_CASTERS_PER_PIXEL_CEILING: u32 = 16;

    /// Clamp `max_shadow_casters_per_pixel` into `1..=CEILING`.
    pub fn clamped_k(&self) -> u32 {
        self.max_shadow_casters_per_pixel
            .clamp(1, Self::MAX_SHADOW_CASTERS_PER_PIXEL_CEILING)
    }
}
