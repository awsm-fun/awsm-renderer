//! Material **prep** pass — the shared, material-independent per-pixel resolve
//! that Plan B introduces (`docs/plans/deferred-shared-prep-pass.md`).
//!
//! It runs once over the visibility buffer (after classify, before per-material
//! shading) and materializes everything that is the *same regardless of
//! material*: world position, interpolated UV sets + vertex colors, and the
//! per-pixel shadow-visibility terms. Per-material kernels then read those
//! buffers instead of recomputing them, so the per-material module shrinks.
//!
//! Stage 0 lands [`PrepPassConfig`] — the build-time knobs every later stage
//! keys on. The pass shader scaffold lives in [`shader`]; its buffers, bind
//! groups, and dispatch wiring arrive in the subsequent stages (see the spec's
//! "Implementation stages").

pub mod bind_group;
pub mod render_pass;
pub mod shader;

/// Max UV sets materialized by the prep pass (Stage 2a). `prep_uv` is a
/// `texture_2d_array` with this many layers; `cs_prep` writes layers
/// `0..min(uv_set_count, MAX_PREP_UV_SETS)`. glTF content almost never exceeds
/// 2 UV sets, so 4 is generous; a material referencing a set `>= cap` clamps to
/// the last layer on read (slim shader, Stage 2b) — bounded + benign.
pub const MAX_PREP_UV_SETS: u32 = 4;

/// Max vertex-color sets materialized by the prep pass (Stage 2a). `prep_vcolor`
/// is a `texture_2d_array` with this many layers. Vertex colors beyond set 0 are
/// vanishingly rare; 2 is generous.
pub const MAX_PREP_COLOR_SETS: u32 = 2;

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

    /// Number of `Rgba8unorm` array layers for the shadow-visibility buffer
    /// (Stage 3): 4 shadow slots are packed per texel (one per channel), so the
    /// layer count is `ceil(K / 4)`. Slot `j` → layer `j / 4`, channel `j % 4`.
    /// Packing keeps the buffer at 4 bytes/px for the default K=4 (vs an
    /// `R32float` K-array's 4·K bytes/px), preserving decision #2's 4K-bandwidth
    /// safety. `R8unorm` storage is avoided (it needs the optional
    /// `r8unorm-storage` WebGPU feature; `Rgba8unorm` is core-guaranteed).
    pub fn shadow_visibility_layers(&self) -> u32 {
        self.clamped_k().div_ceil(4)
    }
}
