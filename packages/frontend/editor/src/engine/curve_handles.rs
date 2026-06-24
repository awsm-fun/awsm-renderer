//! Viewport drag-handles for **curve control points**, backed by
//! `awsm_renderer_web_shared::viewport3d::PointHandleSet`.
//!
//! Mirrors `gizmo.rs`: the handle set lives in a thread-local so both the render
//! loop (per-frame screen-constant zoom) and the canvas pointer handlers (pick +
//! drag) can reach it. When a single `NodeKind::Curve` is selected we spawn one
//! cyan sphere handle per control point (in world space); dragging a handle moves
//! that control point. Live drags edit the node kind directly (so the polyline +
//! Inspector follow); the pointer-up commit dispatches one undoable `SetKind`.

use std::cell::RefCell;

use awsm_renderer_editor_protocol::{CurveDef, NodeKind};
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_web_shared::viewport3d::point_handle::PointHandleSet;
use futures_signals::map_ref;
use glam::{Mat4, Vec3};

use super::bridge::bridge;
use super::context::renderer_handle;
use crate::controller::controller;
use crate::engine::scene::NodeId;
use crate::prelude::*;

thread_local! {
    static HANDLES: RefCell<Option<PointHandleSet>> = const { RefCell::new(None) };
    /// The curve node the handles currently track (`None` when no curve is shown).
    static TARGET: RefCell<Option<NodeId>> = const { RefCell::new(None) };
    /// `(node, CurveDef at drag start)` so the pointer-up commit records the
    /// correct undo inverse (start → final).
    static DRAG_START: RefCell<Option<(NodeId, CurveDef)>> = const { RefCell::new(None) };
}

/// Initialise the handle set. Call once after the renderer + bridge are ready.
pub fn init() {
    HANDLES.with(|h| *h.borrow_mut() = Some(PointHandleSet::new()));
    start_selection_observer();
}

/// Rebuild the handle set whenever the selection (or the scene) changes: show one
/// handle per control point for a single selected curve, hide otherwise. Keyed on
/// `scene.revision` too so the handles re-anchor after an Inspector edit and once
/// the bridge entry (transform key) materializes.
fn start_selection_observer() {
    spawn_local(async move {
        let selected_id =
            controller().selected.signal_ref(
                |ids| {
                    if ids.len() == 1 {
                        Some(ids[0])
                    } else {
                        None
                    }
                },
            );
        map_ref! {
            let id = selected_id,
            let _rev = controller().scene.revision.signal() => *id
        }
        .for_each(|id| async move {
            sync_selection(id).await;
        })
        .await;
    });
}

/// The selected curve's `(node id, def, world matrix-source transform key)`, or
/// `None` if the selection isn't a single curve.
fn curve_target(
    id: Option<NodeId>,
) -> Option<(NodeId, CurveDef, awsm_renderer::transforms::TransformKey)> {
    let id = id?;
    let bridge = bridge();
    let nodes = bridge.nodes.lock().unwrap();
    let entry = nodes.get(&id)?;
    match entry.node.kind.get_cloned() {
        NodeKind::Curve(def) => Some((id, def, entry.transform_key)),
        _ => None,
    }
}

async fn sync_selection(id: Option<NodeId>) {
    // Never rebuild mid-drag — the active drag owns the handle positions, and
    // tearing down its mesh would break the in-flight `MeshKey`.
    let dragging = HANDLES.with(|h| h.borrow().as_ref().is_some_and(|s| s.is_dragging()));
    if dragging {
        return;
    }
    let target = curve_target(id);

    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    HANDLES.with(|h| {
        let mut guard = h.borrow_mut();
        let Some(set) = guard.as_mut() else {
            return;
        };
        match target {
            Some((id, def, tk)) => {
                let world = world_matrix(&renderer, tk);
                let pts: Vec<Vec3> = def
                    .control_points
                    .iter()
                    .map(|p| world.transform_point3(Vec3::from_array(*p)))
                    .collect();
                let _ = set.set_points(&mut renderer, &pts);
                set.show(&mut renderer, true);
                TARGET.with(|t| *t.borrow_mut() = Some(id));
            }
            None => {
                set.clear(&mut renderer);
                TARGET.with(|t| *t.borrow_mut() = None);
            }
        }
    });
}

fn world_matrix(renderer: &AwsmRenderer, tk: awsm_renderer::transforms::TransformKey) -> Mat4 {
    renderer
        .transforms
        .get_world(tk)
        .copied()
        .unwrap_or(Mat4::IDENTITY)
}

/// Whether control-point handles are currently shown (a curve is selected). The
/// canvas uses this to decide whether to GPU-probe a press even when the
/// transform gizmo is disabled.
pub fn has_active_handles() -> bool {
    HANDLES.with(|h| {
        h.borrow()
            .as_ref()
            .is_some_and(|s| s.is_visible() && s.handle_count() > 0)
    })
}

/// Per-frame: keep every handle a fixed pixel size. Called from the render loop
/// after `update_camera` (alongside the gizmo's update).
pub fn per_frame_update(renderer: &mut AwsmRenderer) {
    HANDLES.with(|h| {
        if let Some(set) = h.borrow_mut().as_mut() {
            if let Some(matrices) = renderer.camera.last_matrices.as_ref().cloned() {
                let _ = set.zoom_handles(renderer, &matrices);
            }
        }
    });
}

/// Begin a control-point drag if `mesh_key` is (or the cursor is near) a handle.
/// Returns `true` when a handle was grabbed; the caller then routes pointer-move
/// to [`drag`]. The renderer lock is held by the caller.
pub fn try_start_pick(
    renderer: &mut AwsmRenderer,
    mesh_key: Option<MeshKey>,
    x: i32,
    y: i32,
) -> bool {
    let started = HANDLES.with(|h| {
        let mut guard = h.borrow_mut();
        let Some(set) = guard.as_mut() else {
            return false;
        };
        if !set.is_visible() || set.handle_count() == 0 {
            return false;
        }
        // Exact GPU hit, else a small screen-space tolerance (the spheres are
        // only ~8px, so a near-miss should still grab).
        let idx = mesh_key
            .and_then(|mk| set.is_handle_mesh(mk))
            .or_else(|| set.pick_with_tolerance(renderer, x, y, 12.0));
        if let Some(idx) = idx {
            set.start_drag(renderer, idx, x, y);
            true
        } else {
            false
        }
    });
    if started {
        begin_drag();
    }
    started
}

fn begin_drag() {
    let id = TARGET.with(|t| *t.borrow());
    let Some(id) = id else {
        return;
    };
    let def =
        bridge()
            .nodes
            .lock()
            .unwrap()
            .get(&id)
            .and_then(|n| match n.node.kind.get_cloned() {
                NodeKind::Curve(d) => Some(d),
                _ => None,
            });
    if let Some(def) = def {
        DRAG_START.with(|d| *d.borrow_mut() = Some((id, def)));
    }
}

/// Apply a pointer-move delta to the in-flight handle drag: move the handle, then
/// write the new control-point position back into the node kind (local space) so
/// the polyline + Inspector update live. Not dispatched — the undoable commit
/// happens on pointer-up. The renderer lock is held by the caller.
pub fn drag(renderer: &mut AwsmRenderer, dx: i32, dy: i32) {
    let moved = HANDLES.with(|h| {
        h.borrow_mut()
            .as_mut()
            .and_then(|s| s.update_drag(renderer, dx, dy))
    });
    let Some((idx, world_pos)) = moved else {
        return;
    };
    let id = TARGET.with(|t| *t.borrow());
    let Some(id) = id else {
        return;
    };
    // Convert the dragged world position back into the curve's local space (the
    // control points are stored local; `materialize_curve_viz` bakes the parent
    // world in for rendering).
    let node = bridge().nodes.lock().unwrap().get(&id).map(|n| {
        (
            n.node.clone(),
            world_matrix(renderer, n.transform_key).inverse(),
        )
    });
    let Some((node, inv_world)) = node else {
        return;
    };
    let local = inv_world.transform_point3(world_pos);
    if let NodeKind::Curve(mut d) = node.kind.get_cloned() {
        if let Some(p) = d.control_points.get_mut(idx) {
            *p = local.to_array();
            // Direct set (not a dispatch): re-materializes the polyline + ticks
            // the Inspector signals without bumping `scene.revision` — so the
            // selection observer won't rebuild handles mid-drag.
            node.kind.set(NodeKind::Curve(d));
        }
    }
}

/// On pointer-up: dispatch the net edit as one undoable `SetKind` (reset to the
/// drag-start def first so the controller records start → final), and end the
/// drag.
pub fn commit_drag() {
    let start = DRAG_START.with(|d| d.borrow_mut().take());
    HANDLES.with(|h| {
        if let Some(set) = h.borrow_mut().as_mut() {
            set.end_drag();
        }
    });
    let Some((id, start_def)) = start else {
        return;
    };
    spawn_local(async move {
        let node = bridge()
            .nodes
            .lock()
            .unwrap()
            .get(&id)
            .map(|n| n.node.clone());
        let Some(node) = node else {
            return;
        };
        let NodeKind::Curve(final_def) = node.kind.get_cloned() else {
            return;
        };
        if final_def == start_def {
            return;
        }
        // Reset to the start value so the controller captures start → final.
        node.kind.set(NodeKind::Curve(start_def));
        let _ = controller()
            .dispatch(EditorCommand::SetKind {
                id,
                kind: Box::new(NodeKind::Curve(final_def)),
            })
            .await;
    });
}
