//! Serializable node descriptors used by the invertible scene commands.
//!
//! `InsertSpec` names a fresh node to create (the ribbon's Insert options).
//! `NodeSpec` is a full serializable capture of an existing node subtree — it's
//! how `Delete`'s inverse round-trips the exact removed subtree (same ids) back
//! into the scene on undo, and it's the shape the query snapshot + TOML
//! persistence build on.
//!
//! Pure data only. The reactive materialization (`InsertSpec → Node`,
//! `NodeSpec ↔ Node`) lives in the editor (it touches the live scene graph), so
//! the editor provides those as free functions; here we keep the data + the
//! pure-data conversions (`NodeSpec ↔ EditorNode`, `NodeSpec → NodeQuery`).

use serde::{Deserialize, Serialize};

use awsm_renderer_scene::{EditorNode, LightKind, NodeId, NodeKind, PrimitiveShape, Trs};

/// A fresh node to insert (one per ribbon Insert action). The editor's
/// `build_insert` turns this into a reactive `Node`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum InsertSpec {
    Empty,
    Light(LightKind),
    Camera,
    CollisionBox,
    CollisionSphere,
    CollisionCapsule,
    CollisionCylinder,
    CollisionCone,
    CollisionEllipsoid,
    Primitive(PrimitiveShape),
    Curve,
    Line,
    Sprite,
    Particle,
    Decal,
    Sweep,
    Instances,
    Instancer,
    Mesh,
}

/// A full serializable capture of a node subtree (preserves ids).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct NodeSpec {
    pub id: NodeId,
    pub name: String,
    pub transform: Trs,
    pub kind: NodeKind,
    pub locked: bool,
    pub visible: bool,
    pub prefab: bool,
    pub children: Vec<NodeSpec>,
}

impl NodeSpec {
    /// Convert to the scene-schema's serializable [`EditorNode`] (project.toml
    /// persistence). The two are field-identical; this is a structural map.
    pub fn to_editor_node(&self) -> EditorNode {
        EditorNode {
            id: self.id,
            name: self.name.clone(),
            transform: self.transform,
            kind: self.kind.clone(),
            locked: self.locked,
            visible: self.visible,
            prefab: self.prefab,
            children: self.children.iter().map(|c| c.to_editor_node()).collect(),
        }
    }

    /// Build a `NodeSpec` from a persisted [`EditorNode`].
    pub fn from_editor_node(node: &EditorNode) -> Self {
        Self {
            id: node.id,
            name: node.name.clone(),
            transform: node.transform,
            kind: node.kind.clone(),
            locked: node.locked,
            visible: node.visible,
            prefab: node.prefab,
            children: node.children.iter().map(Self::from_editor_node).collect(),
        }
    }

    /// A lightweight projection for the query snapshot (no transform payload).
    pub fn to_query(&self) -> NodeQuery {
        NodeQuery {
            id: self.id.to_string(),
            name: self.name.clone(),
            kind: kind_tag(&self.kind).to_string(),
            visible: self.visible,
            locked: self.locked,
            children: self.children.iter().map(|c| c.to_query()).collect(),
        }
    }
}

/// A node as projected into the serializable editor snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeQuery {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Whether the node is shown (the Outliner eye toggle).
    #[serde(default = "default_true_nq")]
    pub visible: bool,
    /// Whether the node is locked from selection/editing.
    #[serde(default)]
    pub locked: bool,
    pub children: Vec<NodeQuery>,
}

fn default_true_nq() -> bool {
    true
}

/// A short stable tag for a node kind (used by the query projection).
pub fn kind_tag(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Group => "group",
        NodeKind::Light(_) => "light",
        NodeKind::Collider(_) => "collider",
        NodeKind::Camera(_) => "camera",
        NodeKind::Mesh { .. } => "mesh",
        NodeKind::SkinnedMesh { .. } => "skinned_mesh",
        NodeKind::ClusterMesh { .. } => "cluster_mesh",
        NodeKind::Curve(_) => "curve",
        NodeKind::InstancesAlongCurve(_) => "instances",
        NodeKind::Instancer(_) => "instancer",
        NodeKind::Line(_) => "line",
        NodeKind::Sprite(_) => "sprite",
        NodeKind::ParticleEmitter(_) => "particle",
        NodeKind::Decal(_) => "decal",
    }
}
