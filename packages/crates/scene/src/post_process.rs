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
}

impl Default for PostProcessConfig {
    fn default() -> Self {
        Self {
            tonemapping: ToneMappingConfig::default(),
            bloom: false,
            dof: false,
            exposure: 0.0,
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
