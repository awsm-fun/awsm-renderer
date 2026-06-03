//! Editor-side wiring for the `awsm_renderer_editor::point_handle` gizmo.
//!
//! Responsibilities:
//! - Watch the selection. When exactly one node is selected and it's a
//!   `Curve` or `Line`, populate the `PointHandleSet` with one handle per
//!   control point (in world space). Otherwise, hide.
//! - Re-anchor handles whenever the bound node's transform or control
//!   points change.
//! - Translate handle drags back into authored `control_points` /
//!   `points[i].pos` mutations on the corresponding `Mutable<NodeKind>`.
//! - Per-frame: zoom handles to a fixed pixel size.

use crate::context::{with_renderer, with_renderer_mut};
use crate::scene::{Node, NodeId, NodeKind};
use crate::state::{app_state, PointHandleTarget};
use awsm_renderer::AwsmRenderer;
use futures_signals::map_ref;
use futures_signals::signal::SignalExt;
use glam::{Mat4, Vec3};

/// Spawn the selection / kind observer chain. Called once from
/// `renderer_bridge::init`.
pub fn start() {
    let state = app_state();
    let selected = state.selected.clone();
    let bridge = state.renderer_bridge.clone();

    wasm_bindgen_futures::spawn_local(async move {
        let selected_id = selected.signal_ref(|set| {
            if set.len() == 1 {
                set.iter().next().copied()
            } else {
                None
            }
        });
        // Re-evaluate on bridge revision so a fresh-inserted node syncs once
        // its bridge entry appears.
        map_ref! {
            let id = selected_id,
            let _rev = bridge.nodes_revision.signal() => *id
        }
        .dedupe()
        .for_each(|id| async move {
            sync_for_selection(id).await;
        })
        .await;
    });
}

/// Re-evaluate which (if any) node the point handles should be bound to,
/// and rebuild the handle set from the new target's control points.
async fn sync_for_selection(node_id: Option<NodeId>) {
    let state = app_state();

    let target = node_id.and_then(|id| {
        let scene_nodes = state.scene.nodes.lock_ref();
        let node = find_node_recursive(&scene_nodes, id)?;
        let kind = node.kind.get_cloned();
        match kind {
            NodeKind::Curve(_) => Some(PointHandleTarget::Curve(id)),
            NodeKind::Line(_) => Some(PointHandleTarget::Line(id)),
            _ => None,
        }
    });

    state.point_handle_target.set(target);

    match target {
        Some(target) => {
            // Read the parent's world matrix from inside the same lock
            // hold as set_points / show, so the per-frame snapshot is
            // never racing a concurrent renderer mutation.
            with_renderer_mut(move |r| {
                let world = parent_world_matrix(r, target_node_id(target));
                let positions = world_positions_for_target(target, world);
                let state = app_state();
                let mut set = state.point_handles.lock().unwrap();
                let _ = set.set_points(r, &positions);
                set.show(r, true);
            })
            .await;

            // Watch the node's kind so edits to control points or the
            // transform re-anchor the handles. We drop the prior watcher
            // first by storing a single async-loader on the bridge.
            spawn_kind_watcher(target);
        }
        None => {
            with_renderer_mut(|r| {
                let state = app_state();
                let mut set = state.point_handles.lock().unwrap();
                set.show(r, false);
                set.clear(r);
            })
            .await;
        }
    }
}

fn spawn_kind_watcher(target: PointHandleTarget) {
    let state = app_state();
    let bridge = state.renderer_bridge.clone();
    let target_id = match target {
        PointHandleTarget::Curve(id) | PointHandleTarget::Line(id) => id,
    };
    let node = {
        let scene_nodes = state.scene.nodes.lock_ref();
        find_node_recursive(&scene_nodes, target_id)
    };
    let Some(node) = node else { return };

    wasm_bindgen_futures::spawn_local(async move {
        node.kind
            .signal_cloned()
            .for_each(move |kind| {
                let target = target;
                async move {
                    // If a drag is in flight, skip the re-anchor so we don't
                    // fight the user. The drag-end commit will eventually
                    // round-trip through here.
                    {
                        let state = app_state();
                        let set = state.point_handles.lock().unwrap();
                        if set.is_dragging() {
                            return;
                        }
                    }
                    let kind = kind.clone();
                    with_renderer_mut(move |r| {
                        let world = parent_world_matrix(r, target_node_id(target));
                        let positions = world_positions_from_kind(&kind, world);
                        let state = app_state();
                        let mut set = state.point_handles.lock().unwrap();
                        let _ = set.set_points(r, &positions);
                    })
                    .await;
                }
            })
            .await;
        let _ = bridge; // keep alive
    });
}

fn target_node_id(target: PointHandleTarget) -> NodeId {
    match target {
        PointHandleTarget::Curve(id) | PointHandleTarget::Line(id) => id,
    }
}

/// Build world-space handle positions for a target by reading its
/// current authored kind. Callers pass the parent's world matrix that
/// they already snapshotted under the renderer lock — keeps this
/// function pure + sync.
fn world_positions_for_target(target: PointHandleTarget, parent_world: Mat4) -> Vec<Vec3> {
    // O(1) bridge-map lookup. Was a full-tree DFS via
    // `find_node_recursive`; called every frame from `per_frame_update`
    // whenever a Curve/Line is selected. A deep scene tree was paying
    // a per-frame re-scan for an O(1) hash answer.
    let bridge = super::node_sync::bridge();
    let Some(entry) = bridge
        .nodes
        .lock()
        .unwrap()
        .get(&target_node_id(target))
        .cloned()
    else {
        return Vec::new();
    };
    let kind = entry.node.kind.get_cloned();
    world_positions_from_kind(&kind, parent_world)
}

fn world_positions_from_kind(kind: &NodeKind, parent_world: Mat4) -> Vec<Vec3> {
    let local_positions: Vec<Vec3> = match kind {
        NodeKind::Curve(c) => c
            .control_points
            .iter()
            .map(|p| Vec3::from_array(*p))
            .collect(),
        NodeKind::Line(l) => l.points.iter().map(|p| Vec3::from_array(p.pos)).collect(),
        _ => Vec::new(),
    };
    local_positions
        .into_iter()
        .map(|p| parent_world.transform_point3(p))
        .collect()
}

/// Read the renderer's current world matrix for the bridge entry that
/// represents `node_id`. Sync — caller must already hold the renderer
/// lock (e.g. inside `with_renderer_mut`'s closure or `per_frame_update`).
/// Returns `Mat4::IDENTITY` if the bridge entry or transform is missing.
fn parent_world_matrix(renderer: &AwsmRenderer, node_id: NodeId) -> Mat4 {
    let state = app_state();
    let bridge = state.renderer_bridge.clone();
    let nodes = bridge.nodes.lock().unwrap();
    let Some(entry) = nodes.get(&node_id) else {
        return Mat4::IDENTITY;
    };
    let tk = entry.transform_key;
    drop(nodes);
    renderer
        .transforms
        .get_world(tk)
        .copied()
        .unwrap_or(Mat4::IDENTITY)
}

fn find_node_recursive(
    nodes: &[std::sync::Arc<Node>],
    target: NodeId,
) -> Option<std::sync::Arc<Node>> {
    for n in nodes.iter() {
        if n.id == target {
            return Some(n.clone());
        }
        let children = n.children.lock_ref();
        if let Some(found) = find_node_recursive(&children, target) {
            return Some(found);
        }
    }
    None
}

/// Apply a drag result back into the authored node's control point.
/// Called from the canvas pointer-move handler after `update_drag`
/// returns the new world position.
///
/// Async because we need the parent's world matrix from the renderer —
/// snapshotting it under the lock is correct regardless of who else is
/// touching the renderer, whereas the old sync `try_lock` could silently
/// fall back to `Mat4::IDENTITY` mid-drag.
pub async fn apply_drag(target: PointHandleTarget, handle_index: usize, world_pos: Vec3) {
    let state = app_state();
    let node_id = target_node_id(target);
    let scene_nodes = state.scene.nodes.lock_ref();
    let Some(node) = find_node_recursive(&scene_nodes, node_id) else {
        return;
    };
    drop(scene_nodes);

    // Convert world → local via the parent's inverse world matrix —
    // snapshot under the renderer lock.
    let world = with_renderer(move |r| parent_world_matrix(r, node_id)).await;
    let inv = world.inverse();
    let local = inv.transform_point3(world_pos);

    let mut kind = node.kind.get_cloned();
    let changed = match &mut kind {
        NodeKind::Curve(c) if handle_index < c.control_points.len() => {
            c.control_points[handle_index] = local.to_array();
            true
        }
        NodeKind::Line(l) if handle_index < l.points.len() => {
            l.points[handle_index].pos = local.to_array();
            true
        }
        _ => false,
    };
    if changed {
        node.kind.set(kind);
    }
}

/// Per-frame: re-anchor handles + zoom to a fixed pixel size. Called from
/// the editor's render loop after camera matrices have been updated.
pub fn per_frame_update(renderer: &mut AwsmRenderer) {
    let state = app_state();
    // Re-anchor in case the parent node's world transform changed but the
    // kind didn't fire (e.g. an ancestor node moved). Skip when dragging.
    if let Some(target) = state.point_handle_target.get() {
        let mut set = state.point_handles.lock().unwrap();
        if !set.is_dragging() && set.is_visible() {
            let world = parent_world_matrix(renderer, target_node_id(target));
            let positions = world_positions_for_target(target, world);
            let _ = set.set_points(renderer, &positions);
        }
    }
    let Some(matrices) = renderer.camera.last_matrices.as_ref().cloned() else {
        return;
    };
    let set = state.point_handles.lock().unwrap();
    let _ = set.zoom_handles(renderer, &matrices);
}
