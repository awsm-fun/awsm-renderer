#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightConfig {
    Directional {
        color: [f32; 3],
        intensity: f32,
    },
    Point {
        color: [f32; 3],
        intensity: f32,
        range: f32,
    },
    Spot {
        color: [f32; 3],
        intensity: f32,
        range: f32,
        inner_angle: f32,
        outer_angle: f32,
    },
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

    pub fn default_for(kind: LightKind) -> Self {
        match kind {
            LightKind::Directional => Self::Directional {
                color: [1.0, 1.0, 1.0],
                intensity: 4.0,
            },
            LightKind::Point => Self::Point {
                color: [1.0, 1.0, 1.0],
                intensity: 60.0,
                range: 20.0,
            },
            LightKind::Spot => Self::Spot {
                color: [1.0, 1.0, 1.0],
                intensity: 80.0,
                range: 25.0,
                inner_angle: 0.35,
                outer_angle: 0.7,
            },
        }
    }
}
