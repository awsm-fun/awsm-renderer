//! Serializable node descriptors used by the invertible scene commands.
//!
//! `InsertSpec` names a fresh node to create (the ribbon's Insert options).
//! `NodeSpec` is a full serializable capture of an existing node subtree — it's
//! how `Delete`'s inverse round-trips the exact removed subtree (same ids) back
//! into the scene on undo, and it's the shape the query snapshot + TOML
//! persistence build on.

use serde::{Deserialize, Serialize};

use awsm_web_shared::prelude::{Mutable, MutableVec};
use std::sync::Arc;

use crate::engine::scene::node::Node;
use crate::engine::scene::types::{AssetStatus, NodeKind, Trs};
use crate::engine::scene::NodeId;

use awsm_scene_schema::{LightKind, PrimitiveShape};

/// A fresh node to insert (one per ribbon Insert action).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    Mesh,
}

impl InsertSpec {
    /// Build the reactive `Node` for this spec (fresh id).
    pub fn build(&self) -> Arc<Node> {
        match self {
            InsertSpec::Empty => Node::new_group("Empty"),
            InsertSpec::Light(kind) => Node::new_light(*kind),
            InsertSpec::Camera => Node::new_camera("Camera"),
            InsertSpec::CollisionBox => Node::new_collision_box("Box Collider"),
            InsertSpec::CollisionSphere => Node::new_collision_sphere("Sphere Collider"),
            InsertSpec::CollisionCapsule => Node::new_collision_capsule("Capsule Collider"),
            InsertSpec::CollisionCylinder => Node::new_collision_cylinder("Cylinder Collider"),
            InsertSpec::CollisionCone => Node::new_collision_cone("Cone Collider"),
            InsertSpec::CollisionEllipsoid => Node::new_collision_ellipsoid("Ellipsoid Collider"),
            InsertSpec::Primitive(shape) => {
                Node::new_primitive(primitive_label(shape), shape.clone())
            }
            InsertSpec::Curve => Node::new_curve("Curve"),
            InsertSpec::Line => Node::new_line("Line"),
            InsertSpec::Sprite => Node::new_sprite("Sprite"),
            InsertSpec::Particle => Node::new_particle("Particle Emitter"),
            InsertSpec::Decal => Node::new_decal("Decal"),
            InsertSpec::Sweep => Node::new_sweep("Sweep"),
            InsertSpec::Instances => Node::new_instances("Instances"),
            InsertSpec::Mesh => Node::new_mesh("Mesh"),
        }
    }
}

fn primitive_label(shape: &PrimitiveShape) -> &'static str {
    match shape {
        PrimitiveShape::Plane { .. } => "Plane",
        PrimitiveShape::Box { .. } => "Box",
        PrimitiveShape::Sphere { .. } => "Sphere",
        PrimitiveShape::Cylinder { .. } => "Cylinder",
        PrimitiveShape::Cone { .. } => "Cone",
        PrimitiveShape::Torus { .. } => "Torus",
    }
}

/// A full serializable capture of a node subtree (preserves ids).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Capture an existing node + its subtree.
    pub fn from_node(node: &Node) -> Self {
        Self {
            id: node.id,
            name: node.name.get_cloned(),
            transform: node.transform.get(),
            kind: node.kind.get_cloned(),
            locked: node.locked.get(),
            visible: node.visible.get(),
            prefab: node.prefab.get(),
            children: node
                .children
                .lock_ref()
                .iter()
                .map(|c| NodeSpec::from_node(c))
                .collect(),
        }
    }

    /// Rebuild the reactive node subtree, preserving every id.
    pub fn to_node(&self) -> Arc<Node> {
        Arc::new(Node {
            id: self.id,
            name: Mutable::new(self.name.clone()),
            transform: Mutable::new(self.transform),
            kind: Mutable::new(self.kind.clone()),
            children: MutableVec::new_with_values(
                self.children.iter().map(|c| c.to_node()).collect(),
            ),
            expanded: Mutable::new(true),
            asset_status: Mutable::new(AssetStatus::Idle),
            locked: Mutable::new(self.locked),
            visible: Mutable::new(self.visible),
            prefab: Mutable::new(self.prefab),
        })
    }

    /// Convert to the scene-schema's serializable [`EditorNode`] (project.toml
    /// persistence). The two are field-identical; this is a structural map.
    pub fn to_editor_node(&self) -> awsm_scene_schema::EditorNode {
        awsm_scene_schema::EditorNode {
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
    pub fn from_editor_node(node: &awsm_scene_schema::EditorNode) -> Self {
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
    pub children: Vec<NodeQuery>,
}

/// A short stable tag for a node kind (used by the query projection).
pub fn kind_tag(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Group => "group",
        NodeKind::Model(_) => "model",
        NodeKind::Light(_) => "light",
        NodeKind::Collider(_) => "collider",
        NodeKind::Camera(_) => "camera",
        NodeKind::Primitive { .. } => "primitive",
        NodeKind::Mesh { .. } => "mesh",
        NodeKind::Curve(_) => "curve",
        NodeKind::SweepAlongCurve { .. } => "sweep",
        NodeKind::InstancesAlongCurve(_) => "instances",
        NodeKind::Line(_) => "line",
        NodeKind::Sprite(_) => "sprite",
        NodeKind::ParticleEmitter(_) => "particle",
        NodeKind::Decal(_) => "decal",
    }
}
