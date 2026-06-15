//! Editor-side reactive materialization for the serializable node descriptors.
//!
//! The *data* types (`InsertSpec`, `NodeSpec`, `NodeQuery`, `kind_tag`) live in
//! [`awsm_editor_protocol`] and are re-exported here at their established path.
//! What stays editor-side is the half the protocol crate can't express: turning
//! a spec into — and capturing one from — the live reactive scene graph
//! (`Node`), which carries `Mutable`/`MutableVec` fields and UI-only
//! `AssetStatus`.

use std::sync::Arc;

use awsm_web_shared::prelude::{Mutable, MutableVec};

use crate::engine::scene::node::Node;
use crate::engine::scene::types::AssetStatus;

pub use awsm_editor_protocol::{InsertSpec, NodeQuery, NodeSpec};

/// Build the reactive `Node` for an insert spec (fresh id).
///
/// The procedural-geometry specs (`Primitive` / `Sweep`) are **not** handled
/// here: they mint a backing `MeshDef` asset + bake its cache, which needs scene
/// access, so the controller's `Insert` apply intercepts them via
/// `build_mesh_insert` before this is reached. They're `unreachable!` here.
pub fn build_insert(spec: &InsertSpec) -> Arc<Node> {
    match spec {
        InsertSpec::Empty => Node::new_group("Empty"),
        InsertSpec::Light(kind) => Node::new_light(*kind),
        InsertSpec::Camera => Node::new_camera("Camera"),
        InsertSpec::CollisionBox => Node::new_collision_box("Box Collider"),
        InsertSpec::CollisionSphere => Node::new_collision_sphere("Sphere Collider"),
        InsertSpec::CollisionCapsule => Node::new_collision_capsule("Capsule Collider"),
        InsertSpec::CollisionCylinder => Node::new_collision_cylinder("Cylinder Collider"),
        InsertSpec::CollisionCone => Node::new_collision_cone("Cone Collider"),
        InsertSpec::CollisionEllipsoid => Node::new_collision_ellipsoid("Ellipsoid Collider"),
        InsertSpec::Primitive(_) | InsertSpec::Sweep => {
            unreachable!("Primitive/Sweep inserts are handled by build_mesh_insert")
        }
        InsertSpec::Curve => Node::new_curve("Curve"),
        InsertSpec::Line => Node::new_line("Line"),
        InsertSpec::Sprite => Node::new_sprite("Sprite"),
        InsertSpec::Particle => Node::new_particle("Particle Emitter"),
        InsertSpec::Decal => Node::new_decal("Decal"),
        InsertSpec::Instances => Node::new_instances("Instances"),
        InsertSpec::Mesh => Node::new_mesh("Mesh"),
    }
}

/// Capture an existing node + its subtree as a serializable [`NodeSpec`].
pub fn spec_from_node(node: &Node) -> NodeSpec {
    NodeSpec {
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
            .map(|c| spec_from_node(c))
            .collect(),
    }
}

/// Rebuild the reactive node subtree from a [`NodeSpec`], preserving every id.
pub fn node_from_spec(spec: &NodeSpec) -> Arc<Node> {
    Arc::new(Node {
        id: spec.id,
        name: Mutable::new(spec.name.clone()),
        transform: Mutable::new(spec.transform),
        kind: Mutable::new(spec.kind.clone()),
        children: MutableVec::new_with_values(spec.children.iter().map(node_from_spec).collect()),
        expanded: Mutable::new(true),
        asset_status: Mutable::new(AssetStatus::Idle),
        locked: Mutable::new(spec.locked),
        visible: Mutable::new(spec.visible),
        prefab: Mutable::new(spec.prefab),
    })
}
