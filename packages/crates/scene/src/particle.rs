//! Authored particle emitter definitions.

use super::primitive::TextureRef;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[derive(Copy)]
pub enum SpawnShapeDef {
    Point,
    Sphere {
        radius: f32,
    },
    Cone {
        angle_radians: f32,
        /// Spawn direction in the emitter's **local** space (rotated by the
        /// node's transform). Default `[0,1,0]` shoots particles up the local +Y.
        direction: [f32; 3],
    },
}

impl SpawnShapeDef {
    pub fn default_cone() -> Self {
        // Default direction +Y so a fresh emitter shoots particles
        // upward (smoke / sparks / steam — the typical case). The
        // earlier -Y default ran particles through the ground plane
        // and they were invisible to a fresh user.
        Self::Cone {
            angle_radians: 0.4,
            direction: [0.0, 1.0, 0.0],
        }
    }
}

impl Default for SpawnShapeDef {
    fn default() -> Self {
        Self::default_cone()
    }
}

/// A per-frame force applied to live particles. Serializes externally-tagged:
/// `{"gravity": {"acceleration": [x,y,z]}}` or
/// `{"linear_drag": {"coefficient_x1000": <u32>}}` (drag coefficient ×1000, so
/// `500` = 0.5/s). World-space acceleration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[derive(Copy)]
pub enum ForceDef {
    Gravity { acceleration: [f32; 3] },
    LinearDrag { coefficient_x1000: u32 },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[derive(Eq, Hash, Copy, Default)]
pub enum EmitterSpaceDef {
    /// Particles persist in world space.
    World,
    /// Particles follow the emitter transform.
    #[default]
    Local,
}

/// Externally-tagged: `{"const": [r,g,b,a]}` or
/// `{"linear": {"start": [r,g,b,a], "end": [r,g,b,a]}}`. Alpha is the only
/// transparency knob (see `ParticleEmitterDef::color_over_life`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ColorOverLifeDef {
    Const([f32; 4]),
    Linear { start: [f32; 4], end: [f32; 4] },
}

/// Externally-tagged: `{"const": <f32>}` or `{"linear": {"start": <f32>, "end": <f32>}}`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[derive(Copy)]
pub enum SizeOverLifeDef {
    Const(f32),
    Linear { start: f32, end: f32 },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ParticleEmitterDef {
    pub spawn_rate: f32,
    pub burst_count: u32,
    pub max_alive: u32,
    pub one_shot: bool,
    pub space: EmitterSpaceDef,
    pub shape: SpawnShapeDef,
    pub initial_speed: [f32; 2],
    pub lifetime: [f32; 2],
    pub size: [f32; 2],
    pub forces: Vec<ForceDef>,
    /// Per-particle RGBA curve over lifetime. The alpha channel is
    /// the *only* transparency knob — there used to be a separate
    /// `alpha_over_life: AlphaOverLifeDef` field, but it just
    /// multiplied with this `.a` and trivially produced α² fades
    /// when the user set both to 1→0. The schema no longer carries
    /// it; the fragment shader multiplies the texture's alpha by
    /// this color's `.a` and the per-instance attr alpha and that's
    /// the visible transparency.
    pub color_over_life: ColorOverLifeDef,
    pub size_over_life: SizeOverLifeDef,
    /// Optional sprite texture for billboard rendering.
    pub texture: Option<TextureRef>,
    /// Route this emitter through the transparent-blend pass instead of the
    /// opaque-emissive path. Required for true alpha-fading particles
    /// (smoke, soft glows). Opaque is the default since the visibility
    /// buffer is cheaper.
    #[serde(default)]
    pub blend: bool,
}

impl Default for ParticleEmitterDef {
    fn default() -> Self {
        Self {
            spawn_rate: 60.0,
            burst_count: 0,
            max_alive: 256,
            one_shot: false,
            space: EmitterSpaceDef::Local,
            shape: SpawnShapeDef::default(),
            initial_speed: [1.0, 2.0],
            // A 2s upper bound on lifetime keeps a fresh smoke / steam
            // emitter visible long enough to read the curve falloff —
            // 0.8s was too short for the particles to register before
            // they faded.
            lifetime: [0.4, 2.0],
            size: [0.1, 0.2],
            forces: vec![],
            // Default is a neutral white→white fade so a newly
            // inserted emitter with no texture renders as plain
            // dots, and the user's first texture binding shows up
            // un-tinted. (The old fiery orange→red default made
            // every fresh smoke emitter look like fire.)
            color_over_life: ColorOverLifeDef::Linear {
                start: [1.0, 1.0, 1.0, 1.0],
                end: [1.0, 1.0, 1.0, 0.0],
            },
            size_over_life: SizeOverLifeDef::Linear {
                start: 1.0,
                end: 0.3,
            },
            texture: None,
            blend: false,
        }
    }
}
