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
    /// Depth-warp exponent for EVSM. Higher values give better contact
    /// hardening at the cost of overflow risk in `RGBA16F`.
    pub evsm_exponent: f32,
    /// Half-width of the separable Gaussian blur applied to the EVSM
    /// moments, in texels.
    pub evsm_blur_radius: u32,
    /// Maximum number of point lights that can cast shadows
    /// simultaneously. Sets the cube-array slice count.
    pub max_point_shadows: u32,
    /// Tints each directional cascade range so the splits are visible
    /// in the editor. Drives a debug bitmask flag in the opaque pass.
    pub debug_cascade_colors: bool,
}

impl Default for ShadowsConfig {
    fn default() -> Self {
        Self {
            sscs_enabled: true,
            sscs_step_count: 16,
            atlas_size: 4096,
            evsm_atlas_size: 2048,
            evsm_exponent: 20.0,
            evsm_blur_radius: 3,
            max_point_shadows: 8,
            debug_cascade_colors: false,
        }
    }
}
