//! Renderer-wide post-processing settings serialized into the project +
//! player bundle.
//!
//! Mirrors the runtime `awsm_renderer::post_process::PostProcessing` (the
//! schema stays renderer-independent, like [`crate::shadows::ShadowsConfig`]).
//! The editor renders it in the Settings drawer's Post-processing section and
//! syncs it live via `settings_sync`; the player applies it at scene load in
//! `scene-loader::populate_awsm_scene` via `AwsmRenderer::set_post_processing`.
//!
//! Every field has a `#[serde(default)]` initialiser matching the RENDERER
//! defaults, so projects authored before the schema gained a `post_process`
//! block round-trip cleanly and apply as a no-op.
//!
//! Depth of field's focus distance / aperture are PER-CAMERA renderer state
//! (`CameraMatrices`), not part of this global block — `dof` here only gates
//! the effect pass. Per-camera focus knobs are a follow-on.

/// Mirrors `awsm_renderer::post_process::PostProcessing`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PostProcessConfig {
    /// Tonemapping operator applied in the display pass.
    #[serde(default)]
    pub tonemapping: ToneMappingConfig,
    /// Bloom (bright-pass blur composited pre-tonemap). Toggling recompiles
    /// the effects pipelines.
    #[serde(default)]
    pub bloom: bool,
    /// Depth of field. Uses the active camera's `focus_distance` / `aperture`.
    /// Toggling recompiles the effects pipelines.
    #[serde(default)]
    pub dof: bool,
    /// Pre-tonemap scene exposure in EV (stops). 0 = unity, +1 = 2× brighter,
    /// -1 = half. Live uniform (no recompile).
    #[serde(default)]
    pub exposure: f32,
    /// Bloom bright-pass threshold in pre-exposure HDR luminance — pixels
    /// brighter than this glow. Live uniform (no recompile).
    #[serde(default = "default_bloom_threshold")]
    pub bloom_threshold: f32,
    /// Bloom soft-knee width below the threshold (smooth fade-in). Live uniform.
    #[serde(default = "default_bloom_knee")]
    pub bloom_knee: f32,
    /// Bloom mix strength over the scene. Live uniform.
    #[serde(default = "default_bloom_intensity")]
    pub bloom_intensity: f32,
    /// Bloom scatter — biases the glow toward wider/softer mips. Live uniform.
    #[serde(default = "default_bloom_scatter")]
    pub bloom_scatter: f32,
    /// Screen-space reflections. Nested so it round-trips through
    /// project.toml ⇄ scene.toml automatically; off by default (zero cost).
    #[serde(default)]
    pub ssr: SsrConfig,
}

fn default_bloom_threshold() -> f32 {
    1.0
}
fn default_bloom_knee() -> f32 {
    0.5
}
fn default_bloom_intensity() -> f32 {
    1.0
}
fn default_bloom_scatter() -> f32 {
    1.0
}

impl Default for PostProcessConfig {
    fn default() -> Self {
        Self {
            tonemapping: ToneMappingConfig::default(),
            bloom: false,
            dof: false,
            exposure: 0.0,
            bloom_threshold: default_bloom_threshold(),
            bloom_knee: default_bloom_knee(),
            bloom_intensity: default_bloom_intensity(),
            bloom_scatter: default_bloom_scatter(),
            ssr: SsrConfig::default(),
        }
    }
}

/// Screen-space reflections configuration. Nested in [`PostProcessConfig`].
///
/// SSR reflects the actual on-screen (opaque) geometry off glossy surfaces,
/// falling back to IBL specular where a ray misses. Reflectance is
/// **material-owned** (each material writes a `{mask, spread, tint}` descriptor
/// into its shading output) — this config only carries the global/pass-level
/// knobs, never a per-material "roughness".
///
/// `enabled = false` (the default) records no pass and allocates no targets, so
/// pre-SSR projects round-trip and cost nothing.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SsrConfig {
    /// Master toggle. `false` ⇒ SSR pass not recorded, targets not allocated.
    #[serde(default)]
    pub enabled: bool,
    /// Reflection strength multiplier (~0..2). Live uniform.
    #[serde(default = "default_ssr_intensity")]
    pub intensity: f32,
    /// Maximum ray length in world units. Live uniform.
    #[serde(default = "default_ssr_max_distance")]
    pub max_distance: f32,
    /// View-space depth band (world units) a ray must cross to register a hit —
    /// prevents reflecting through thin geometry. Live uniform.
    #[serde(default = "default_ssr_thickness")]
    pub thickness: f32,
    /// Linear-march step budget (the fallback trace / short rays). Live uniform.
    #[serde(default = "default_ssr_max_steps")]
    pub max_steps: u32,
    /// Skip SSR above this reflection spread (0 mirror … 1 diffuse); hands off to
    /// IBL. Live uniform.
    #[serde(default = "default_ssr_spread_cutoff")]
    pub spread_cutoff: f32,
    /// Screen-border fade width (0..1) hiding the screen-space seam. Live uniform.
    #[serde(default = "default_ssr_edge_fade")]
    pub edge_fade: f32,
    /// Trace resolution scale: 0.5 = half-res + upsample, 1.0 = full. Structural
    /// (selects a compiled variant → recompiles).
    #[serde(default = "default_ssr_resolution_scale")]
    pub resolution_scale: f32,
    /// Temporal accumulation (reproject + neighbourhood-clamp). Structural
    /// (recompiles). Off until the temporal milestone lands.
    #[serde(default)]
    pub temporal: bool,
    /// History blend weight (0..1) when `temporal` is on. Live uniform.
    #[serde(default = "default_ssr_temporal_weight")]
    pub temporal_weight: f32,
    /// Debug visualization (0 off, 1 confidence, 2 travel, 3 source,
    /// 4 traversal steps). DEV-ONLY and transient — never persisted.
    #[serde(skip)]
    pub debug: u32,
    /// Software-BVH reflections: real off-screen hits replace the probe/env
    /// fallback for SSR misses on near-mirror pixels. Structural
    /// (recompiles + builds the bvh_trace pass). A HIGH-END tier — default
    /// off; persisted like `temporal`.
    #[serde(default)]
    pub bvh_reflections: bool,
}

fn default_ssr_intensity() -> f32 {
    1.0
}
fn default_ssr_max_distance() -> f32 {
    100.0
}
fn default_ssr_thickness() -> f32 {
    1.0
}
fn default_ssr_max_steps() -> u32 {
    96
}
fn default_ssr_spread_cutoff() -> f32 {
    0.6
}
fn default_ssr_edge_fade() -> f32 {
    0.1
}
fn default_ssr_resolution_scale() -> f32 {
    0.5
}
fn default_ssr_temporal_weight() -> f32 {
    0.9
}

impl Default for SsrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: default_ssr_intensity(),
            max_distance: default_ssr_max_distance(),
            thickness: default_ssr_thickness(),
            max_steps: default_ssr_max_steps(),
            spread_cutoff: default_ssr_spread_cutoff(),
            edge_fade: default_ssr_edge_fade(),
            resolution_scale: default_ssr_resolution_scale(),
            temporal: false,
            temporal_weight: default_ssr_temporal_weight(),
            debug: 0,
            bvh_reflections: false,
        }
    }
}

/// Mirrors `awsm_renderer::post_process::ToneMapping`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum ToneMappingConfig {
    /// No tonemapping (linear → output). HDR values clip.
    None,
    /// The Khronos PBR-neutral operator — the renderer default.
    #[default]
    KhronosNeutralPbr,
    /// ACES filmic.
    Aces,
}
