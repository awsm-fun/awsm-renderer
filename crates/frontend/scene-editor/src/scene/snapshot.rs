//! Conversion between the editor's live `Scene` (with `Mutable<T>` fields
//! for reactive editing) and the serializable `EditorProject` defined in
//! `lockstep-game-data`.
//!
//! `SceneSnapshot` is a type alias here purely for backwards-compatibility
//! with existing call sites — the on-disk format and the in-memory
//! snapshot used by undo/redo are the same `EditorProject` struct.

use crate::scene::{node::Node, types::AssetStatus, Scene};
use awsm_scene_schema::{EditorNode, EditorProject};
use std::sync::Arc;

/// The editor's snapshot type — same shape as the on-disk format.
pub type SceneSnapshot = EditorProject;

/// Capture the live `Scene` into an `EditorProject`.
pub fn capture(scene: &Scene) -> EditorProject {
    let environment = scene.environment.get_cloned();
    let shadows = scene.shadows.get_cloned();
    let assets = scene.assets.lock().unwrap().clone();
    // Phase 5 of the dynamic-materials plan will populate this from the
    // editor's reactive `Scene::custom_materials` mutable; Phase 1's
    // snapshot keeps it empty so the schema field round-trips cleanly
    // without yet wiring an editor-side store.
    let custom_materials: Vec<awsm_scene_schema::CustomMaterialRef> = Vec::new();
    let nodes = scene
        .nodes
        .lock_ref()
        .iter()
        .map(|node| capture_node(node))
        .collect();
    EditorProject {
        // Project name lives in AppState (not the reactive Scene) and
        // is written into the EditorProject by the Save flow — kept
        // out of capture/apply so rename doesn't enter the history
        // ring and stays orthogonal to undo/redo.
        name: String::new(),
        environment,
        shadows,
        assets,
        custom_materials,
        nodes,
    }
}

/// Replace the live scene's contents with an `EditorProject` in-place.
pub fn apply_to(snapshot: &EditorProject, scene: &Scene) {
    scene.environment.set(snapshot.environment.clone());
    scene.shadows.set(snapshot.shadows.clone());
    *scene.assets.lock().unwrap() = snapshot.assets.clone();
    let mut lock = scene.nodes.lock_mut();
    lock.clear();
    for snap in &snapshot.nodes {
        lock.push_cloned(hydrate_node(snap));
    }
}

fn capture_node(node: &Node) -> EditorNode {
    EditorNode {
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
            .map(|child| capture_node(child))
            .collect(),
    }
}

fn hydrate_node(snap: &EditorNode) -> Arc<Node> {
    use futures_signals::signal::Mutable;
    use futures_signals::signal_vec::MutableVec;
    Arc::new(Node {
        id: snap.id,
        name: Mutable::new(snap.name.clone()),
        transform: Mutable::new(snap.transform),
        kind: Mutable::new(snap.kind.clone()),
        children: MutableVec::new_with_values(snap.children.iter().map(hydrate_node).collect()),
        expanded: Mutable::new(true),
        asset_status: Mutable::new(AssetStatus::Idle),
        locked: Mutable::new(snap.locked),
        visible: Mutable::new(snap.visible),
        prefab: Mutable::new(snap.prefab),
    })
}
