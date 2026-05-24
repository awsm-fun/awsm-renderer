//! Renderer-wide shadow configuration.

/// Renderer-wide shadow settings. Independent of any individual light;
/// drives atlas sizing, EVSM atlas allocation, the SSCS global toggle,
/// and the cube-pool capacity.
///
/// Changes are picked up on the next call to `Shadows::set_config`.
/// `atlas_size` changes trigger a re-pack at the start of next frame;
/// `max_point_shadows` changes are expensive (full cube-array
/// re-create) and should be applied sparingly.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowsConfig {
    /// Enables the screen-space contact-shadow multiplier on the
    /// directional shadow term.
    pub sscs_enabled: bool,
    /// Number of screen-space ray-march steps for SSCS.
    pub sscs_step_count: u32,
    /// Width / height (square) of the 2D PCF/PCSS shadow atlas in
    /// texels. Must be a power of two.
    pub atlas_size: u32,
    /// Width / height of the EVSM RGBA16F atlas in texels. Allocated
    /// lazily on the first frame an EVSM cascade is requested.
    pub evsm_atlas_size: u32,
    /// Per-layer dimension (square) of the directional-cascade texture
    /// array in texels. One layer per cascade — a 2K layer covers a
    /// 4-cascade light in 64 MB (Depth32f). Per-light `resolution`
    /// authoring is treated as a hint: a cascade smaller than this is
    /// rendered into the top-left sub-rect of its layer; a cascade
    /// larger than this is clamped to the layer size. Per-layer
    /// render-attachment views let throttled cascades skip the depth
    /// pass without disturbing other cascades.
    pub cascade_resolution: u32,
    /// Maximum simultaneously-active directional cascade layers in the
    /// texture array. With up to 4 cascades per directional light, 16
    /// layers covers four directional shadow casters — far more than
    /// the scene usually has, but cheap (`cascade_resolution²` × 4 B
    /// per layer).
    pub cascade_array_max_layers: u32,
    /// Depth-warp exponent for EVSM. Higher values give better contact
    /// hardening at the cost of overflow risk in `RGBA16F`.
    pub evsm_exponent: f32,
    /// Half-width of the separable Gaussian blur applied to the EVSM
    /// moments, in texels.
    pub evsm_blur_radius: u32,
    /// Maximum number of point lights that can cast shadows
    /// simultaneously. Sets the cube-array slice count.
    pub max_point_shadows: u32,
    /// Per-face cube shadow map resolution in texels (square). Memory
    /// cost is `4 · res² · 6 · max_point_shadows` bytes (Depth32f). The
    /// default (`1024`) costs ~24 MB at `max_point_shadows = 8` — sane
    /// for desktops; mobile-class browsers may prefer `512` (6 MB) or
    /// `256` (1.5 MB). Must be a power of two ≥ 64.
    ///
    /// Changing this at runtime re-allocates the cube pool and triggers
    /// a bind-group recreate; do it sparingly.
    pub point_shadow_resolution: u32,
    /// Tints each directional cascade range so the splits are visible
    /// in the editor. Drives a debug bitmask flag in the opaque pass.
    pub debug_cascade_colors: bool,
    /// PCSS / soft-shadow tap count at the *closest* (`dist_ratio = 0`)
    /// receiver-to-light distance. Higher = smoother contact shadows on
    /// near surfaces. The WGSL kernel uses a static `for i in 0..16u`
    /// loop with a dynamic `break` so values above 16 silently clamp.
    /// Drives both the blocker search and the variable-kernel PCF in
    /// `sample_shadow_cube` and `sample_shadow_cascade_array`.
    pub pcss_max_taps: u32,
    /// PCSS / soft-shadow tap count at the *farthest* (`dist_ratio = 1`)
    /// receiver-to-light distance. Lower = cheaper distant shadows.
    /// Values below 4 risk visible Poisson noise; 4 is the AAA-typical
    /// floor for far-cascade PCF. Linearly interpolated against
    /// `pcss_max_taps` by `pcss_tap_count` in `bind_groups.wgsl`.
    pub pcss_min_taps: u32,
}

impl Default for ShadowsConfig {
    fn default() -> Self {
        Self {
            sscs_enabled: false,
            sscs_step_count: 16,
            atlas_size: 4096,
            evsm_atlas_size: 2048,
            cascade_resolution: 2048,
            cascade_array_max_layers: 16,
            // 10 is the AAA-canon EVSM exponent for fp16 — gives a
            // smooth contact-hardening curve with comfortable
            // half-float headroom. 20 (the prior default) was at the
            // top of the fp16 range and the resulting Chebyshev curve
            // was so sharp it rendered like a binary mask. See
            // `EVSM_EXPONENT_MAX_FP16` for the hard cap.
            evsm_exponent: 10.0,
            // 6 gives a clearly soft far cascade. Lower values
            // (3 was the prior default) leave EVSM visually similar
            // to PCF for typical caster sizes.
            evsm_blur_radius: 6,
            max_point_shadows: 8,
            point_shadow_resolution: 1024,
            debug_cascade_colors: false,
            // 16 = the max the Poisson table holds and the historical
            // PCSS quality bar. Anything more would need a longer table.
            pcss_max_taps: 16,
            // 4 = the typical floor: enough taps to keep distant
            // penumbras smooth without visible Poisson noise. Drops the
            // per-PCSS-pixel fragment cost by ~4× on distant receivers
            // vs the historical fixed-16 path.
            pcss_min_taps: 4,
        }
    }
}

impl ShadowsConfig {
    /// Hard upper safe limit for `evsm_exponent` under `RGBA16F`
    /// moment storage. The moments `exp(c · z)` are evaluated for
    /// `z ∈ [-1, 1]`, so the largest stored value is `exp(c) ≈ 5·10⁸`
    /// at `c = 20` — already at the very top of the half-float range.
    /// Pushing higher silently saturates and produces near-binary
    /// (hard-edged) Chebyshev visibility, which defeats the whole
    /// point of EVSM. AAA tunings sit near `c ≈ 10` for fp16.
    pub const EVSM_EXPONENT_MAX_FP16: f32 = 18.0;
}
