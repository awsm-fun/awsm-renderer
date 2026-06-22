//! Authored polylines.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct LinePoint {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct LineDef {
    pub points: Vec<LinePoint>,
    /// Line width in CSS pixels. Rendered by the screen-space fat-line
    /// pipeline (`awsm_renderer::add_line_strip`) — a fixed pixel width
    /// regardless of camera distance.
    pub width_px: f32,
    /// If true, the line is depth-tested with `Always` so it draws over everything.
    /// Useful for in-editor curve handles and debug overlays.
    pub depth_test_always: bool,
}

impl Default for LineDef {
    fn default() -> Self {
        Self {
            points: vec![
                LinePoint {
                    pos: [0.0, 0.0, 0.0],
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                LinePoint {
                    pos: [1.0, 0.0, 0.0],
                    color: [1.0, 1.0, 1.0, 1.0],
                },
            ],
            width_px: 2.5,
            depth_test_always: false,
        }
    }
}
