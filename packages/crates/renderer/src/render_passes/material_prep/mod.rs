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
pub mod buffers;
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
/// `BucketConfig`). The shared prep pass is now unconditional; this config
/// only carries the `K` sizing knob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrepPassConfig {
    /// `K` — the maximum shadow casters that can overlap a *single pixel* (NOT
    /// total scene casters). Sizes the per-pixel shadow-visibility buffer to `K`
    /// layers; the j-th shadowed light in a pixel's froxel, in froxel-list
    /// order, writes layer `j`. Overflow (>K shadowed lights over one pixel) is
    /// clamped + logged. Default 4.
    pub max_shadow_casters_per_pixel: u32,
    // NOTE (decision #2): world position is NEVER materialized — the slim shader
    // always reconstructs it from depth (`get_standard_coordinates`). The former
    // `reconstruct_world_pos` tunable was therefore obsolete and removed (Stage 6
    // cleanup).
    /// SSCS shader-variant inputs, mirrored from `ShadowsConfig` (kept in sync by
    /// `AwsmRenderer::set_shadows_config`). They live here — not read from the
    /// shadow uniform — because both drive the shadow module's COMPILE-TIME
    /// template: `sscs_enabled` folds into the `apply_sscs` capability gate
    /// (`sscs_available`) so a disabled config emits zero SSCS code, and
    /// `sscs_step_count` is baked as the ray-march loop bound (unroll-friendly,
    /// no per-fragment counter). `PrepPassConfig` is already the shadow-shader
    /// config threaded to every shadow-consuming pipeline build (opaque, prep,
    /// edge-resolve), so co-locating them here reaches all cache-key sites
    /// without new plumbing. Changing either re-keys + recompiles those pipelines
    /// (via `mark_variants_dirty` + `commit_load`); the SSCS *scalar* tuning
    /// params stay live uniforms in `ShadowsConfig` / `ShadowGlobals`.
    pub sscs_enabled: bool,
    pub sscs_step_count: u32,
}

impl Default for PrepPassConfig {
    fn default() -> Self {
        Self {
            max_shadow_casters_per_pixel: 4,
            // Match `ShadowsConfig::default()` (SSCS off; 16-step march) so the
            // initial pipeline build and the shadow config agree before any
            // `set_shadows_config` sync runs.
            sscs_enabled: false,
            sscs_step_count: 16,
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
