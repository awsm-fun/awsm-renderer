//! Renderer-wide shadow settings serialized into `EditorProject`.
//!
//! Mirrors the runtime `awsm_renderer::shadows::ShadowsConfig`. The
//! editor renders it in the Environment-tab Shadows panel; non-editor
//! consumers (players, headless builds) read it from `project.json`
//! and feed it into the renderer at startup via
//! `AwsmRenderer::set_shadows_config(cfg.into())`.
//!
//! Every field has a `#[serde(default)]` initialiser so projects
//! authored before the schema gained a `shadows` block round-trip
//! cleanly — load picks up defaults; the next save writes the
//! resolved values.
//!
//! Every field — including resource-shape ones (`atlas_size`,
//! `evsm_atlas_size`, `max_point_shadows`, `point_shadow_resolution`)
//! — applies on the next `write_gpu`. Resource-shape changes incur a
//! GPU texture + bind group recreate so don't poke them at frame
//! rate, but from editor inspectors / level-load they're free.

/// Mirrors `awsm_renderer::shadows::config::ShadowsConfig`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShadowsConfig {
    /// Master toggle for the screen-space contact-shadow multiplier
    /// applied to the dominant directional light's term.
    #[serde(default = "default_sscs_enabled")]
    pub sscs_enabled: bool,
    /// Number of screen-space ray-march steps for SSCS. Higher = more
    /// faithful contact darkening at the cost of fragment work.
    #[serde(default = "default_sscs_step_count")]
    pub sscs_step_count: u32,
    /// World-space length of each SSCS ray-march step, in metres. Total
    /// reach = `sscs_step_world · sscs_step_count`. World-space (not
    /// pixel-space) so the same surface point samples the same world
    /// positions every frame regardless of camera zoom.
    #[serde(default = "default_sscs_step_world")]
    pub sscs_step_world: f32,
    /// SSCS occluder-slab thickness in metres: a depth-buffer texel this
    /// far or less in front of the ray counts as an occluder. Larger
    /// admits thicker casters (a resting ball) at the cost of over-
    /// darkening behind thin geometry.
    #[serde(default = "default_sscs_thickness")]
    pub sscs_thickness: f32,
    /// Maximum SSCS darkening for the DIRECTIONAL shadow term (0..1).
    /// Conservative by default — directional SSCS is a refinement on top
    /// of a cascade map that already covers the contact.
    #[serde(default = "default_sscs_directional_darkening")]
    pub sscs_directional_darkening: f32,
    /// Maximum SSCS darkening for PUNCTUAL (point/spot) shadow terms
    /// (0..1). Higher than directional because a cube shadow map leaves a
    /// fully-lit contact "Peter-Pan" gap that SSCS must actually fill.
    #[serde(default = "default_sscs_punctual_darkening")]
    pub sscs_punctual_darkening: f32,
    /// 2D atlas size (square) for the PCF / spot / EVSM-source depth
    /// passes. Must be a power of two. The atlas auto-grows when the
    /// row-pack allocator overflows (capped at 8192).
    #[serde(default = "default_atlas_size")]
    pub atlas_size: u32,
    /// EVSM atlas size (square). Moments are stored at `RGBA16F`, so
    /// memory cost is `8 · size²` bytes — 2048² ≈ 32 MB. Set to 1 if
    /// you never use EVSM.
    #[serde(default = "default_evsm_atlas_size")]
    pub evsm_atlas_size: u32,
    /// Depth-warp exponent for EVSM. Higher gives crisper contact
    /// hardening; over ~25 risks `RGBA16F` overflow.
    #[serde(default = "default_evsm_exponent")]
    pub evsm_exponent: f32,
    /// Gaussian blur half-width in texels applied to EVSM moments.
    /// Clamped to `MAX_BLUR_RADIUS` (8) on the GPU side.
    #[serde(default = "default_evsm_blur_radius")]
    pub evsm_blur_radius: u32,
    /// Maximum number of point lights that can cast shadows
    /// simultaneously. Sizes the cube-array slot pool.
    #[serde(default = "default_max_point_shadows")]
    pub max_point_shadows: u32,
    /// Per-face cube shadow resolution in texels (square). Each light
    /// uses ~`24 · res²` bytes of VRAM at this size.
    #[serde(default = "default_point_shadow_resolution")]
    pub point_shadow_resolution: u32,
    /// Tint each directional cascade range so split boundaries are
    /// visible during authoring.
    #[serde(default)]
    pub debug_cascade_colors: bool,
    // NOTE: the shadow-denoise blur is intentionally NOT persisted here.
    // It's a renderer-runtime quality knob (`renderer::ShadowsConfig::denoise`,
    // default on) toggled live in the editor's Settings drawer, exactly like
    // MSAA — none of the renderer-wide shadow config is wired scene→renderer in
    // the editor yet. If/when that subsystem lands, add `denoise` back alongside
    // its siblings with real round-tripping rather than as a dangling field.
}

impl Default for ShadowsConfig {
    fn default() -> Self {
        Self {
            sscs_enabled: default_sscs_enabled(),
            sscs_step_count: default_sscs_step_count(),
            sscs_step_world: default_sscs_step_world(),
            sscs_thickness: default_sscs_thickness(),
            sscs_directional_darkening: default_sscs_directional_darkening(),
            sscs_punctual_darkening: default_sscs_punctual_darkening(),
            atlas_size: default_atlas_size(),
            evsm_atlas_size: default_evsm_atlas_size(),
            evsm_exponent: default_evsm_exponent(),
            evsm_blur_radius: default_evsm_blur_radius(),
            max_point_shadows: default_max_point_shadows(),
            point_shadow_resolution: default_point_shadow_resolution(),
            debug_cascade_colors: false,
        }
    }
}

fn default_sscs_enabled() -> bool {
    // Off by default: SSCS is an opinionated, artefact-prone
    // contact-shadow refinement (visible under grazing angles and
    // when scene scale doesn't match the world-space reach). Users
    // enable it explicitly per project from the Shadows… panel.
    false
}
fn default_sscs_step_count() -> u32 {
    16
}
fn default_sscs_step_world() -> f32 {
    0.04
}
fn default_sscs_thickness() -> f32 {
    0.05
}
fn default_sscs_directional_darkening() -> f32 {
    0.35
}
fn default_sscs_punctual_darkening() -> f32 {
    0.9
}
fn default_atlas_size() -> u32 {
    4096
}
fn default_evsm_atlas_size() -> u32 {
    2048
}
fn default_evsm_exponent() -> f32 {
    // 10 is the AAA-canon value for fp16 — see
    // `awsm_renderer::shadows::config::ShadowsConfig::EVSM_EXPONENT_MAX_FP16`
    // for the hard cap (~18) before half-float saturation collapses
    // the Chebyshev curve into a hard binary mask.
    10.0
}
fn default_evsm_blur_radius() -> u32 {
    6
}
fn default_max_point_shadows() -> u32 {
    8
}
fn default_point_shadow_resolution() -> u32 {
    1024
}
