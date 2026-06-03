//! Emitter configuration: knobs for a single particle source.

use awsm_curves::{Curve1, LinearCurve1};

use crate::spawn::{Force, SpawnShape};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EmitterSpace {
    /// Particles persist in world space (smoke trails, sparks behind a moving object).
    World,
    /// Particles follow the emitter's transform (jet flame on a robot's back).
    Local,
}

#[derive(Debug, Clone)]
pub struct Emitter {
    pub spawn_rate: f32,
    pub burst_count: u32,
    pub max_alive: u32,
    pub one_shot: bool,
    pub space: EmitterSpace,
    pub shape: SpawnShape,
    pub initial_speed: (f32, f32),
    pub lifetime: (f32, f32),
    pub size: (f32, f32),
    pub forces: Vec<Force>,
    /// Per-particle RGBA over its lifetime. The alpha channel
    /// doubles as the transparency curve — multiplied into the
    /// fragment shader's per-instance alpha. There used to be a
    /// separate `alpha_over_life` field, but it was just an extra
    /// scalar that multiplied with this `.a`; collapsing the two
    /// avoids the "α² fade" footgun where users set both to 1→0
    /// and got a quadratic falloff.
    pub color_over_life: ColorOverLife,
    pub size_over_life: SizeOverLife,
}

impl Default for Emitter {
    fn default() -> Self {
        Self {
            spawn_rate: 30.0,
            burst_count: 0,
            max_alive: 256,
            one_shot: false,
            space: EmitterSpace::World,
            shape: SpawnShape::Point,
            initial_speed: (1.0, 2.0),
            lifetime: (0.5, 1.5),
            size: (0.1, 0.2),
            forces: Vec::new(),
            color_over_life: ColorOverLife::Linear {
                start: [1.0, 1.0, 1.0, 1.0],
                end: [1.0, 1.0, 1.0, 0.0],
            },
            size_over_life: SizeOverLife::Const(1.0),
        }
    }
}

/// Simplified curve-1 forms suitable for editor round-trip.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ColorOverLife {
    Const([f32; 4]),
    Linear { start: [f32; 4], end: [f32; 4] },
}

impl ColorOverLife {
    pub fn sample(&self, t: f32) -> [f32; 4] {
        match self {
            ColorOverLife::Const(c) => *c,
            ColorOverLife::Linear { start, end } => LinearCurve1 {
                start: *start,
                end: *end,
            }
            .sample(t.clamp(0.0, 1.0)),
        }
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SizeOverLife {
    Const(f32),
    Linear { start: f32, end: f32 },
}

impl SizeOverLife {
    pub fn sample(&self, t: f32) -> f32 {
        match self {
            SizeOverLife::Const(c) => *c,
            SizeOverLife::Linear { start, end } => LinearCurve1 {
                start: *start,
                end: *end,
            }
            .sample(t.clamp(0.0, 1.0)),
        }
    }
}
