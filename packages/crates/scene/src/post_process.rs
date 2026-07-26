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
    /// Atmospheric haze. Nested like [`SsrConfig`]; off by default (the fog
    /// term isn't even compiled into the effects shader).
    #[serde(default)]
    pub atmosphere: AtmosphereConfig,
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
            atmosphere: AtmosphereConfig::default(),
        }
    }
}

/// Atmospheric haze configuration. Nested in [`PostProcessConfig`].
///
/// A stylized exponential medium — not a physically-derived Rayleigh/Mie sky.
/// Distant geometry fades toward [`color`](Self::color) at a rate set by
/// [`density`](Self::density), optionally thinning with height so a scene can
/// have haze pooling low and clear air above it.
///
/// How the medium is integrated is [`mode`](Self::mode) — a three-way choice,
/// **not** an on/off plus a style flag, because the volumetric path REPLACES
/// the analytic one rather than adding to it (same air; running both would
/// extinguish it twice). `mode` and
/// [`volumetric_temporal`](Self::volumetric_temporal) are the **structural**
/// fields; everything else is a live uniform.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct AtmosphereConfig {
    /// Off / analytic fog / froxel volumetrics. STRUCTURAL — `Off` compiles no
    /// haze term at all.
    #[serde(default)]
    pub mode: AtmosphereMode,
    /// Linear radiance of fully-saturated haze — what an infinitely distant
    /// surface fades to. Live uniform.
    #[serde(default = "default_atmosphere_color")]
    pub color: [f32; 3],
    /// Extinction per meter; the 1/e distance is `1 / density`. Live uniform.
    #[serde(default = "default_atmosphere_density")]
    pub density: f32,
    /// World Y at which density is full. Live uniform.
    #[serde(default)]
    pub base_height: f32,
    /// Exponential thinning per meter above `base_height`. `0` = uniform
    /// medium (no height falloff at all). Live uniform.
    #[serde(default = "default_atmosphere_height_falloff")]
    pub height_falloff: f32,
    /// Henyey-Greenstein phase anisotropy: `0` isotropic, `> 0` forward
    /// scattering (bright halo around a light you look toward), `< 0` back
    /// scattering. Live uniform; only read on the volumetric path.
    #[serde(default = "default_scattering_anisotropy")]
    pub scattering_anisotropy: f32,
    /// Temporally reproject + blend the froxel volume across frames. The volume
    /// is heavily undersampled, so this is what turns banding into smooth haze
    /// — at the cost of ghosting behind fast movers. STRUCTURAL; only
    /// meaningful in [`AtmosphereMode::Volumetric`].
    #[serde(default)]
    pub volumetric_temporal: bool,
}

fn default_atmosphere_color() -> [f32; 3] {
    [0.5, 0.6, 0.7]
}
fn default_atmosphere_density() -> f32 {
    0.02
}
fn default_atmosphere_height_falloff() -> f32 {
    0.0
}
fn default_scattering_anisotropy() -> f32 {
    // Mild forward scatter. Real haze and smoke are strongly forward-scattering,
    // and it's what makes a beam pointed toward the camera flare instead of
    // reading as a flat grey cone.
    0.3
}

/// How atmospheric haze is integrated. Mirrors the renderer's
/// `AtmospherePhase`; a three-way type so the meaningless fourth state of
/// "volumetric but disabled" can't be spelled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum AtmosphereMode {
    /// No haze. The term isn't compiled into the effects shader at all, so
    /// pre-atmosphere projects round-trip and cost nothing.
    #[default]
    Off,
    /// Closed-form fog along the view ray: cheap aerial perspective, and the
    /// right choice for most scenes. No light shafts — it never asks which
    /// lights reach a point in the air.
    Fog,
    /// Froxel scattering volume with per-light in-scatter: beams, shafts, a
    /// pool of lit haze under a fixture. Substantially more expensive; wants
    /// [`AtmosphereConfig::volumetric_temporal`] to look smooth.
    Volumetric,
}

impl Default for AtmosphereConfig {
    fn default() -> Self {
        Self {
            mode: AtmosphereMode::Off,
            color: default_atmosphere_color(),
            density: default_atmosphere_density(),
            base_height: 0.0,
            height_falloff: default_atmosphere_height_falloff(),
            scattering_anisotropy: default_scattering_anisotropy(),
            volumetric_temporal: false,
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
    // 0.04, matching the RENDERER default (renderer/src/post_process.rs) —
    // these diverged when the "reclaim the periphery" retune changed only
    // the renderer side, so every authored scene silently pinned the old
    // 0.1 dead-band and the tuned default never shipped.
    0.04
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `[post_process]` block authored before atmosphere existed must parse
    /// with haze OFF and the renderer defaults on every other field — the
    /// "pre-atmosphere projects round-trip and cost nothing" claim in
    /// [`AtmosphereConfig`]'s docs, held to by a test rather than by hope.
    #[test]
    fn pre_atmosphere_post_process_parses_with_haze_off() {
        let toml_src = r#"
bloom = true
exposure = 1.5
"#;
        let parsed: PostProcessConfig = toml::from_str(toml_src).unwrap();
        assert!(parsed.bloom);
        assert_eq!(parsed.atmosphere, AtmosphereConfig::default());
        assert_eq!(parsed.atmosphere.mode, AtmosphereMode::Off);
    }

    /// `mode` is the on-disk tag for the haze integration, so these names are a
    /// persistence contract exactly like `LightParamKind`'s: rename a variant
    /// and every scene that authored it silently reverts to clear air on the
    /// next load rather than failing loudly.
    #[test]
    fn atmosphere_mode_wire_names_are_stable() {
        for (mode, wire) in [
            (AtmosphereMode::Off, "\"off\""),
            (AtmosphereMode::Fog, "\"fog\""),
            (AtmosphereMode::Volumetric, "\"volumetric\""),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), wire);
            assert_eq!(
                serde_json::from_str::<AtmosphereMode>(wire).unwrap(),
                mode,
                "{wire} must parse back to the variant that wrote it"
            );
        }
    }

    /// An authored haze block survives a project.toml round-trip field for
    /// field. `density`/`height_falloff` in particular are the ones a
    /// serialize-then-reparse gap would silently reset to the defaults,
    /// turning a tuned scene back into clear air.
    #[test]
    fn authored_atmosphere_roundtrips_through_toml() {
        let authored = PostProcessConfig {
            atmosphere: AtmosphereConfig {
                mode: AtmosphereMode::Volumetric,
                color: [0.016, 0.019, 0.028],
                density: 0.008,
                base_height: -1.25,
                height_falloff: 0.05,
                scattering_anisotropy: 0.65,
                volumetric_temporal: true,
            },
            ..PostProcessConfig::default()
        };
        let round_tripped: PostProcessConfig =
            toml::from_str(&toml::to_string(&authored).unwrap()).unwrap();
        assert_eq!(round_tripped.atmosphere, authored.atmosphere);
    }
}
