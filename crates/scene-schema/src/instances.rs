//! Instance-along-curve schema: place copies of a source node along a curve.

use super::tree::NodeId;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InstancesAlongCurveDef {
    pub curve_node: NodeId,
    pub source_node: NodeId,
    pub spacing: f32,
    pub side_offset: f32,
    pub orient_to_tangent: bool,
    /// Per-instance color overrides applied in order; if shorter than the placed
    /// instance count, the last value is repeated.
    pub per_instance_colors: Vec<[f32; 4]>,
}

impl Default for InstancesAlongCurveDef {
    fn default() -> Self {
        Self {
            curve_node: NodeId::default(),
            source_node: NodeId::default(),
            spacing: 1.0,
            side_offset: 0.0,
            orient_to_tangent: true,
            per_instance_colors: vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SweepAlongCurveDef {
    pub curve_node: NodeId,
    pub cross_section: super::curve::CrossSectionDef,
    pub uv_mode: super::curve::SweepUvMode,
    pub up_hint: [f32; 3],
    pub samples: u32,
}

impl Default for SweepAlongCurveDef {
    fn default() -> Self {
        Self {
            curve_node: NodeId::default(),
            cross_section: super::curve::CrossSectionDef::default_tube(),
            uv_mode: super::curve::SweepUvMode::default(),
            up_hint: [0.0, 1.0, 0.0],
            samples: 64,
        }
    }
}
