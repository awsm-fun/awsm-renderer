/// Punctual light configuration for a `NodeKind::Light` node.
///
/// Each variant carries its parametric data plus an inline
/// [`LightShadowConfig`]. The shadow config defaults to "cast on,
/// soft filter, 1024² atlas" so a freshly-authored light renders with
/// shadows out of the box; existing scenes that predate shadow support
/// round-trip cleanly thanks to `#[serde(default)]`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightConfig {
    Directional {
        color: [f32; 3],
        intensity: f32,
        #[serde(default)]
        shadow: LightShadowConfig,
    },
    Point {
        color: [f32; 3],
        intensity: f32,
        range: f32,
        #[serde(default)]
        shadow: LightShadowConfig,
    },
    Spot {
        color: [f32; 3],
        intensity: f32,
        range: f32,
        inner_angle: f32,
        outer_angle: f32,
        #[serde(default)]
        shadow: LightShadowConfig,
    },
}

/// On-disk shadow configuration for a punctual light. Mirrors the
/// runtime `awsm_renderer::shadows::LightShadowParams`; the scene
/// editor converts between them in its renderer bridge.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LightShadowConfig {
    /// Master shadow-cast toggle for this light.
    #[serde(default = "default_true")]
    pub cast: bool,
    /// Constant depth bias added at sample time. Suppresses acne.
    #[serde(default = "default_depth_bias")]
    pub depth_bias: f32,
    /// Receiver-position offset along the surface normal applied
    /// before the comparison sample. Better than slope-scale for
    /// grazing surfaces.
    #[serde(default = "default_normal_bias")]
    pub normal_bias: f32,
    /// Per-cascade / per-face shadow map resolution.
    #[serde(default = "default_shadow_res")]
    pub resolution: u32,
    /// Filter mode at the shading sample site.
    #[serde(default)]
    pub hardness: LightShadowHardness,
    /// Multiplier on the estimated PCSS penumbra size.
    #[serde(default = "default_pcss_scale")]
    pub pcss_penumbra_scale: f32,
    /// Beyond this distance from the camera the shadow fades and the
    /// light skips its shadow pass that frame.
    #[serde(default = "default_max_distance")]
    pub max_distance: f32,
    /// Number of cascades for directional lights (1..=4). Ignored
    /// otherwise.
    #[serde(default = "default_cascades")]
    pub cascade_count: u8,
    /// PSSM split blend λ (0.0 = uniform, 1.0 = logarithmic).
    #[serde(default = "default_cascade_lambda")]
    pub cascade_split_lambda: f32,
    /// Which trailing cascades store EVSM moments instead of PCF.
    #[serde(default)]
    pub evsm_cutoff: EvsmCutoff,
    /// How often the far cascade(s) re-render.
    #[serde(default)]
    pub far_cascade_update_rate: FarCascadeUpdateRate,
    /// How often each cube face of a point-light shadow re-renders.
    /// Ignored for directional / spot lights.
    #[serde(default)]
    pub cube_face_update_rate: CubeFaceUpdateRate,
}

impl Default for LightShadowConfig {
    fn default() -> Self {
        Self {
            cast: true,
            depth_bias: 0.0005,
            normal_bias: 0.05,
            resolution: 1024,
            hardness: LightShadowHardness::Soft,
            pcss_penumbra_scale: 1.0,
            max_distance: 100.0,
            cascade_count: 4,
            cascade_split_lambda: 0.5,
            evsm_cutoff: EvsmCutoff::LastCascade,
            far_cascade_update_rate: FarCascadeUpdateRate::Every4Frames,
            cube_face_update_rate: CubeFaceUpdateRate::EveryFrame,
        }
    }
}

/// Sample-site filter mode for a light's shadow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LightShadowHardness {
    /// 1-tap comparison sample.
    Hard,
    /// Fixed 3x3 PCF kernel.
    #[default]
    Soft,
    /// Blocker-search + variable-kernel PCF. 2D atlas only.
    Pcss,
}

/// How many trailing directional cascades use EVSM instead of PCF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvsmCutoff {
    /// Every cascade uses PCF / PCSS.
    Off,
    /// Only the farthest cascade uses EVSM.
    #[default]
    LastCascade,
    /// The two farthest cascades use EVSM.
    LastTwoCascades,
}

/// Update cadence for the farthest directional cascade. Near cascades
/// always re-render every frame. Default is `Every4Frames` — the far
/// cascade is the most expensive and least sensitive to per-frame
/// updates; the runtime throttle's drift check invalidates the cache
/// whenever the camera or light moves enough to matter, so the visual
/// hit is invisible on typical scenes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FarCascadeUpdateRate {
    /// Re-render the far cascade every frame.
    EveryFrame,
    /// Re-render the far cascade every 2 frames.
    Every2Frames,
    /// Re-render the far cascade every 4 frames. Default.
    #[default]
    Every4Frames,
    /// Re-render the far cascade every 8 frames.
    Every8Frames,
}

/// Update cadence for the 6 cube faces of a point-light shadow. Mobile
/// browsers / many-light scenes can drop to `Every2Frames` to halve the
/// per-frame cube pass cost — fine for slow-moving lights and casters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CubeFaceUpdateRate {
    /// All 6 faces re-render every frame.
    #[default]
    EveryFrame,
    /// Each cube face re-renders every 2 frames.
    Every2Frames,
    /// Each cube face re-renders every 4 frames.
    Every4Frames,
    /// Each cube face re-renders every 8 frames.
    Every8Frames,
}

fn default_true() -> bool {
    true
}
fn default_depth_bias() -> f32 {
    0.0005
}
fn default_normal_bias() -> f32 {
    0.05
}
fn default_shadow_res() -> u32 {
    1024
}
fn default_pcss_scale() -> f32 {
    1.0
}
fn default_max_distance() -> f32 {
    100.0
}
fn default_cascades() -> u8 {
    4
}
fn default_cascade_lambda() -> f32 {
    0.5
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Eq, Hash, Copy)]
pub enum LightKind {
    Directional,
    Point,
    Spot,
}

impl LightConfig {
    pub fn kind(&self) -> LightKind {
        match self {
            Self::Directional { .. } => LightKind::Directional,
            Self::Point { .. } => LightKind::Point,
            Self::Spot { .. } => LightKind::Spot,
        }
    }

    /// Returns a reference to this light's shadow configuration.
    pub fn shadow(&self) -> &LightShadowConfig {
        match self {
            Self::Directional { shadow, .. }
            | Self::Point { shadow, .. }
            | Self::Spot { shadow, .. } => shadow,
        }
    }

    /// Returns a mutable reference to this light's shadow configuration.
    pub fn shadow_mut(&mut self) -> &mut LightShadowConfig {
        match self {
            Self::Directional { shadow, .. }
            | Self::Point { shadow, .. }
            | Self::Spot { shadow, .. } => shadow,
        }
    }

    pub fn default_for(kind: LightKind) -> Self {
        let shadow = LightShadowConfig::default();
        match kind {
            LightKind::Directional => Self::Directional {
                color: [1.0, 1.0, 1.0],
                intensity: 4.0,
                shadow: LightShadowConfig {
                    resolution: 2048,
                    ..shadow.clone()
                },
            },
            LightKind::Point => Self::Point {
                color: [1.0, 1.0, 1.0],
                intensity: 60.0,
                range: 20.0,
                shadow: shadow.clone(),
            },
            LightKind::Spot => Self::Spot {
                color: [1.0, 1.0, 1.0],
                intensity: 80.0,
                range: 25.0,
                inner_angle: 0.35,
                outer_angle: 0.7,
                shadow: LightShadowConfig {
                    hardness: LightShadowHardness::Hard,
                    ..shadow
                },
            },
        }
    }
}
