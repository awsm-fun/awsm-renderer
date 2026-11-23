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
    /// The anti-aliasing the scene is meant to be seen with. Saved with the
    /// project and carried in the bundle, so the look the author judged the
    /// scene by is the one a player starts with; a host is free to override it
    /// after load (a quality setting, a low-end device).
    #[serde(default)]
    pub anti_aliasing: AntiAliasingConfig,
}

/// Authored anti-aliasing choice. Nested in [`PostProcessConfig`].
///
/// Three modes: MSAA (optionally with SMAA on top, as before TAA existed),
/// SMAA alone, or TAA. `taa: Some(..)` selects TAA and takes precedence: the
/// loader then ignores `msaa` / `smaa` (the renderer rejects TAA combined
/// with either), and the editor saves them `false` alongside it. A file
/// without a `taa` field loads exactly as before.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct AntiAliasingConfig {
    /// 4× multisampling. On by default (the renderer's own default).
    #[serde(default = "default_true")]
    pub msaa: bool,
    /// SMAA post-process anti-aliasing. Off by default; independent of MSAA.
    #[serde(default)]
    pub smaa: bool,
    /// Temporal anti-aliasing (and temporal upscaling below render scale 1).
    /// `None` (absent from the file) = not TAA. (No `skip_serializing_if`:
    /// the editor's bitcode project format cannot skip fields; TOML omits a
    /// `None` on its own, so scene files stay unchanged.)
    #[serde(default)]
    pub taa: Option<TaaSettings>,
}

impl Default for AntiAliasingConfig {
    fn default() -> Self {
        Self {
            msaa: true,
            smaa: false,
            taa: None,
        }
    }
}

/// Authored TAA settings. Mirrors `awsm_renderer::taa::TaaConfig` (the schema
/// stays renderer-independent); every field defaults to the renderer's
/// shipped value, so a file may carry only what it changes.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", default)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TaaSettings {
    /// Render size / output size, 0.5..=1.0. 1.0 = TAA at native resolution;
    /// below it the scene renders smaller and TAA reconstructs the output
    /// (presets: Quality 0.67, Balanced 0.58, Performance 0.5).
    pub render_scale: f32,
    /// Sharpening strength after the resolve (0 = off).
    pub sharpen: f32,
    /// Resolve tuning. Default = the shipped tuning.
    pub tuning: TaaTuningSettings,
}

impl Default for TaaSettings {
    fn default() -> Self {
        Self {
            render_scale: 1.0,
            sharpen: 0.0,
            tuning: TaaTuningSettings::default(),
        }
    }
}

/// TAA resolve tuning. Mirrors `awsm_renderer::taa::TaaTuning` field for
/// field (same defaults; a scene-loader test pins the two together). Every
/// value is a live uniform: changing one never recompiles or resets history.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", default)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TaaTuningSettings {
    /// Variance-clip half-width (standard deviations) when still.
    pub gamma_still: f32,
    /// Variance-clip half-width when moving.
    pub gamma_moving: f32,
    /// Speed (output px / frame) at which the clip box reaches `gamma_moving`.
    pub gamma_moving_speed: f32,
    /// Relative depth slack of the disocclusion test.
    pub disocclusion_threshold: f32,
    /// History-length cap at native resolution.
    pub max_history_native: f32,
    /// History-length cap when upscaling.
    pub max_history_upscaling: f32,
    /// Minimum current-frame weight at `motion_alpha_speed` px / frame.
    pub motion_alpha: f32,
    /// Speed (output px / frame) at which `motion_alpha` is reached.
    pub motion_alpha_speed: f32,
    /// Reactive value written by transparents per unit of alpha.
    pub reactive_transparent: f32,
    /// Reactive value for materials flagged `temporal: reactive`.
    pub reactive_material: f32,
    /// Extra reactivity from SSR reflectivity.
    pub reactive_ssr: f32,
    /// Added to `log2(render_scale)` for the texture LOD bias.
    pub mip_bias_extra: f32,
    /// History-length cap for a pixel moving at `max_history_moving_speed`
    /// or faster (a short memory keeps resampling blur from building up).
    pub max_history_moving: f32,
    /// Speed (output px / frame) at which the cap has dropped from the still
    /// value to `max_history_moving`.
    pub max_history_moving_speed: f32,
    /// Minimum current-frame weight on a motion boundary (a 3×3 holding
    /// surfaces that move differently, e.g. a moving silhouette).
    pub motion_edge_alpha: f32,
    /// Relative speed (output px / frame) between the nearest and farthest
    /// surface of the 3×3 at which `motion_edge_alpha` is reached.
    pub motion_edge_speed: f32,
}

impl Default for TaaTuningSettings {
    fn default() -> Self {
        Self {
            gamma_still: 1.25,
            gamma_moving: 0.75,
            gamma_moving_speed: 2.0,
            disocclusion_threshold: 0.05,
            max_history_native: 16.0,
            max_history_upscaling: 24.0,
            motion_alpha: 0.2,
            motion_alpha_speed: 8.0,
            reactive_transparent: 0.8,
            reactive_material: 0.6,
            reactive_ssr: 0.0,
            mip_bias_extra: 0.0,
            max_history_moving: 4.0,
            max_history_moving_speed: 1.0,
            motion_edge_alpha: 0.6,
            motion_edge_speed: 2.0,
        }
    }
}

fn default_true() -> bool {
    true
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
            anti_aliasing: AntiAliasingConfig::default(),
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
    /// How far the froxel volume extends, in meters. The light-culling grid
    /// this volume borrows its slicing from runs to ~10 km so it can bin
    /// distant lights — marching a 32-slice volume over THAT range gives
    /// kilometre-thick far slices that saturate to solid haze and wash the
    /// whole frame out. So the volume gets its own, much nearer far plane and
    /// simply doesn't simulate the medium past it. Live uniform; volumetric
    /// mode only.
    #[serde(default = "default_volumetric_distance")]
    pub volumetric_distance: f32,
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
fn default_volumetric_distance() -> f32 {
    // ~80 m covers an arena or a club interior, which is what beams are for.
    // Every engine ships a knob like this (Unreal's default is 60 m).
    80.0
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
            volumetric_distance: default_volumetric_distance(),
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
                volumetric_distance: 45.0,
                volumetric_temporal: true,
            },
            ..PostProcessConfig::default()
        };
        let round_tripped: PostProcessConfig =
            toml::from_str(&toml::to_string(&authored).unwrap()).unwrap();
        assert_eq!(round_tripped.atmosphere, authored.atmosphere);
    }

    /// Scenes authored before TAA keep their AA exactly: no `taa` field
    /// parses as `None`, and a non-TAA config writes no `taa` key, so their
    /// files do not change on re-save.
    #[test]
    fn pre_taa_anti_aliasing_is_unchanged() {
        let parsed: PostProcessConfig = toml::from_str(
            r#"
[anti_aliasing]
msaa = false
smaa = true
"#,
        )
        .unwrap();
        assert_eq!(
            parsed.anti_aliasing,
            AntiAliasingConfig {
                msaa: false,
                smaa: true,
                taa: None
            }
        );
        let absent: PostProcessConfig = toml::from_str("bloom = true").unwrap();
        assert_eq!(absent.anti_aliasing, AntiAliasingConfig::default());
        let written = toml::to_string(&PostProcessConfig::default()).unwrap();
        assert!(!written.contains("taa"), "{written}");
    }

    /// A TAA block may carry only what it changes; the rest takes the shipped
    /// defaults, and an authored one round-trips field for field.
    #[test]
    fn taa_settings_default_partially_and_roundtrip() {
        let parsed: PostProcessConfig = toml::from_str(
            r#"
[anti_aliasing]
msaa = false
[anti_aliasing.taa]
render_scale = 0.67
[anti_aliasing.taa.tuning]
gamma_moving = 0.6
"#,
        )
        .unwrap();
        let taa = parsed.anti_aliasing.taa.expect("taa block");
        assert_eq!(taa.render_scale, 0.67);
        assert_eq!(taa.sharpen, 0.0);
        assert_eq!(taa.tuning.gamma_moving, 0.6);
        assert_eq!(
            taa.tuning.gamma_still,
            TaaTuningSettings::default().gamma_still
        );

        let authored = PostProcessConfig {
            anti_aliasing: AntiAliasingConfig {
                msaa: false,
                smaa: false,
                taa: Some(TaaSettings {
                    render_scale: 0.58,
                    sharpen: 0.25,
                    tuning: TaaTuningSettings {
                        max_history_native: 12.0,
                        ..TaaTuningSettings::default()
                    },
                }),
            },
            ..PostProcessConfig::default()
        };
        let round_tripped: PostProcessConfig =
            toml::from_str(&toml::to_string(&authored).unwrap()).unwrap();
        assert_eq!(round_tripped.anti_aliasing, authored.anti_aliasing);
    }
}
