//! Authored 3D curves: control points + closed flag + tension + sample count.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CurveDef {
    /// Catmull-Rom control points in world (or node-local) space.
    pub control_points: Vec<[f32; 3]>,
    pub closed: bool,
    /// Tension parameter for Catmull-Rom. 0.5 is classic; 0.0 is linear; 1.0 is "looser".
    pub tension: f32,
    /// How many samples to use when rasterizing this curve into a polyline for visualization
    /// or feeding into a sweep / instance-placement.
    pub sample_count: u32,
}

impl Default for CurveDef {
    fn default() -> Self {
        Self {
            control_points: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [2.0, 0.0, 1.0],
                [3.0, 0.0, 0.0],
            ],
            closed: false,
            tension: 0.5,
            sample_count: 64,
        }
    }
}

/// Cross-section selection for `SweepAlongCurve`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum CrossSectionDef {
    Strip { width: f32, y_offset: f32 },
    Tube { radius: f32, radial_segments: u32 },
    Wall { width: f32, height: f32 },
    Profile { points: Vec<[f32; 2]>, closed: bool },
}

impl CrossSectionDef {
    pub fn default_strip() -> Self {
        Self::Strip {
            width: 0.3,
            y_offset: 0.0,
        }
    }
    pub fn default_tube() -> Self {
        Self::Tube {
            radius: 0.1,
            radial_segments: 8,
        }
    }
    pub fn default_wall() -> Self {
        Self::Wall {
            width: 0.4,
            height: 0.2,
        }
    }
    pub fn default_profile() -> Self {
        // Small square profile — gives a recognizable extrusion the
        // moment the user flips the variant select, then they edit
        // the points list (currently via project.json — full UI for a
        // points editor is a separate concern).
        Self::Profile {
            points: vec![[0.0, 0.0], [0.3, 0.0], [0.3, 0.3], [0.0, 0.3]],
            closed: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Copy, Default)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum SweepUvMode {
    #[default]
    StretchOnce,
    RepeatByLength {
        u_repeat: f32,
        v_repeat_per_unit: f32,
    },
}
