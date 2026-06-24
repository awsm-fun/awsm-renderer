//! Instance-along-curve schema: place copies of a source node along a curve.

use super::tree::{MeshShadowConfig, NodeId};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct InstancesAlongCurveDef {
    pub curve_node: NodeId,
    pub source_node: NodeId,
    pub spacing: f32,
    pub side_offset: f32,
    pub orient_to_tangent: bool,
    /// Per-instance color overrides applied in order; if shorter than the placed
    /// instance count, the last value is repeated.
    pub per_instance_colors: Vec<[f32; 4]>,
    /// Per-instance shadow cast / receive flags. Applies to every
    /// placed instance.
    #[serde(default)]
    pub shadow: MeshShadowConfig,
}

impl Default for InstancesAlongCurveDef {
    fn default() -> Self {
        Self {
            curve_node: NodeId::nil(),
            source_node: NodeId::nil(),
            spacing: 1.0,
            side_offset: 0.0,
            orient_to_tangent: true,
            per_instance_colors: vec![],
            shadow: MeshShadowConfig::default(),
        }
    }
}

// `SweepAlongCurveDef` is an authoring *recipe* (a `MeshBase::Sweep` payload that
// bakes to a blob) — it lives in `awsm-renderer-meshgen` alongside the sweep evaluator,
// not in the runtime schema.
