//! Per-light shadow parameters (runtime, not on-disk schema).

/// Runtime per-light shadow parameters. The renderer-side counterpart
/// to `scene_schema::LightShadowConfig` — the scene editor converts
/// between them in its renderer-bridge. A non-editor consumer
/// constructs `LightShadowParams` directly via `Default::default()`.
///
/// `cast: false` is the default so a light only gains shadows after an
/// explicit call to `AwsmRenderer::set_light_shadow_params`.
#[derive(Clone, Debug, PartialEq)]
pub struct LightShadowParams {
    /// Master shadow-cast toggle for this light.
    pub cast: bool,
    /// Constant depth offset added at sample time. Pushes the
    /// comparison reference closer to the light to suppress acne.
    pub depth_bias: f32,
    /// Receiver-position offset along the surface normal applied before
    /// the shadow lookup. Cures grazing-angle acne without the Peter
    /// Panning that slope-scale bias produces.
    pub normal_bias: f32,
    /// Per-cascade / per-face shadow map resolution. Directional
    /// lights use this as the base; deeper cascades downscale via
    /// `resolution >> i`.
    pub resolution: u32,
    /// Sample-site filter mode.
    pub hardness: LightShadowHardness,
    /// Multiplier on the estimated PCSS penumbra size. Only consulted
    /// when `hardness == Pcss`.
    pub pcss_penumbra_scale: f32,
    /// Camera-distance fadeout cutoff for this light's shadow.
    pub max_distance: f32,
    /// Number of cascades (1..=4). Directional only; ignored otherwise.
    pub cascade_count: u8,
    /// PSSM blend between uniform (0.0) and logarithmic (1.0) cascade
    /// splits. Directional only; ignored otherwise.
    pub cascade_split_lambda: f32,
    /// How many trailing cascades use EVSM moments instead of PCF.
    /// Directional only.
    pub evsm_cutoff: EvsmCutoff,
    /// Re-render rate for the farthest cascade(s). Directional only.
    pub far_cascade_update_rate: FarCascadeUpdateRate,
}

impl Default for LightShadowParams {
    fn default() -> Self {
        Self {
            cast: false,
            depth_bias: 0.0005,
            normal_bias: 0.05,
            resolution: 1024,
            hardness: LightShadowHardness::Soft,
            pcss_penumbra_scale: 1.0,
            max_distance: 100.0,
            cascade_count: 4,
            cascade_split_lambda: 0.5,
            evsm_cutoff: EvsmCutoff::LastCascade,
            far_cascade_update_rate: FarCascadeUpdateRate::EveryFrame,
        }
    }
}

/// Filter mode at the shadow sample site.
///
/// - `Hard`: 1-tap `textureSampleCompare`.
/// - `Soft`: fixed 3x3 PCF kernel.
/// - `Pcss`: Percentage-Closer Soft Shadows (blocker search +
///   variable-kernel PCF). 2D atlas only; the editor grays it out for
///   point lights.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LightShadowHardness {
    /// 1-tap comparison sample. Crisp; cheapest.
    Hard,
    /// Fixed 3x3 PCF kernel. Balanced default for most lights.
    #[default]
    Soft,
    /// Blocker-search + variable-kernel PCF. Most expensive; reserve for
    /// hero lights or hero shots. 2D atlas only.
    Pcss,
}

/// Which trailing directional cascades store EVSM moments instead of
/// raw depth. The last `N` cascades (per the variant) are promoted; the
/// remaining near cascades stay on PCF / PCSS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EvsmCutoff {
    /// Every cascade uses PCF / PCSS.
    Off,
    /// Only the farthest cascade uses EVSM.
    #[default]
    LastCascade,
    /// The two farthest cascades use EVSM.
    LastTwoCascades,
}

/// Re-render cadence for the farthest directional cascade. Near
/// cascades always re-render every frame; this only throttles the work
/// for distant geometry where per-frame change is small relative to a
/// texel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FarCascadeUpdateRate {
    /// Re-render the far cascade(s) every frame.
    #[default]
    EveryFrame,
    /// Re-render every 2 frames.
    Every2Frames,
    /// Re-render every 4 frames.
    Every4Frames,
    /// Re-render every 8 frames.
    Every8Frames,
}

impl FarCascadeUpdateRate {
    /// Returns the period in frames for this update rate.
    pub fn period(self) -> u64 {
        match self {
            Self::EveryFrame => 1,
            Self::Every2Frames => 2,
            Self::Every4Frames => 4,
            Self::Every8Frames => 8,
        }
    }
}

/// Per-mesh shadow flags. The defaults are derived per-mesh by the
/// scene loader (opaque → cast+receive, transparent → neither); the
/// shadow-pass and shading-side filters consult these.
///
/// Sprite, line, and particle nodes ignore these — they have hardcoded
/// no-cast / no-receive behaviour in v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshShadowFlags {
    /// Whether this mesh shows up in the shadow-generation pass.
    pub cast: bool,
    /// Whether this mesh's shaded pixels darken under shadow lookup.
    pub receive: bool,
}

impl Default for MeshShadowFlags {
    fn default() -> Self {
        Self {
            cast: true,
            receive: true,
        }
    }
}

impl MeshShadowFlags {
    /// Conservative default for transparent materials (no cast, no
    /// receive) — used by the scene loader to derive per-mesh flags
    /// before the user has opted in.
    pub const TRANSPARENT_DEFAULT: Self = Self {
        cast: false,
        receive: false,
    };
}
