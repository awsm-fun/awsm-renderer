//! On-canvas transform gizmo, backed by
//! `awsm_renderer_web_shared::viewport3d::TransformController`.
//!
//! The gizmo is generated procedurally (always-on-top fat lines) and picked
//! analytically — see the controller. It lives in a thread-local (wasm is
//! single-threaded) so both the render loop (per-frame zoom-to-screen-size +
//! re-anchor under the selection) and the canvas pointer handlers (pick + drag)
//! can reach it.

use std::cell::RefCell;

use awsm_renderer_editor_protocol::EditorMode;
use awsm_renderer::transforms::TransformKey;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_web_shared::viewport3d::transform_controller::{
    GizmoSpace, TransformController, TransformObject,
};
use futures_signals::map_ref;

use super::context::renderer_handle;
use crate::controller::controller;
use crate::engine::bridge::bridge;
use crate::engine::scene::NodeId;
use crate::prelude::*;

thread_local! {
    static GIZMO: RefCell<Option<TransformController>> = const { RefCell::new(None) };
}

/// Active manipulation tool — which gizmo handle set is shown. `Select` shows no
/// handles (click-to-select only). Driven by the viewport's tool palette; read
/// each frame by [`per_frame_update`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GizmoMode {
    Select,
    Move,
    Rotate,
    Scale,
    /// Show translate + rotate + scale handles all at once. Each handle still
    /// routes to its own operation (hit-testing keys off the picked handle's
    /// `GizmoKind`, not the mode), so universal mode is purely additive
    /// visibility — see `hidden_for_mode`.
    Universal,
}

thread_local! {
    static GIZMO_MODE: Mutable<GizmoMode> = Mutable::new(GizmoMode::Move);
}

/// The shared gizmo-mode handle (the palette sets it; the gizmo observes it).
pub fn gizmo_mode() -> Mutable<GizmoMode> {
    GIZMO_MODE.with(|m| m.clone())
}

/// `(translation_hidden, rotation_hidden, scale_hidden)` for a mode.
fn hidden_for_mode(mode: GizmoMode) -> (bool, bool, bool) {
    match mode {
        GizmoMode::Select => (true, true, true),
        GizmoMode::Move => (false, true, true),
        GizmoMode::Rotate => (true, false, true),
        GizmoMode::Scale => (true, true, false),
        GizmoMode::Universal => (false, false, false),
    }
}

/// Initialise the gizmo. Call once after the renderer + bridge are ready.
pub fn init() {
    spawn_local(async {
        if let Err(err) = init_inner().await {
            tracing::error!("gizmo init failed: {err}");
        }
    });
}

async fn init_inner() -> Result<(), String> {
    let handle = renderer_handle();
    let mut renderer = handle.lock().await;

    // The gizmo is generated procedurally (fat lines) — no `.glb` to load.
    let mut controller = TransformController::new(&mut renderer, GizmoSpace::Global)
        .map_err(|e| format!("TransformController::new: {e}"))?;

    // Hide every handle until a selection appears.
    let _ = controller.set_hidden(&mut renderer, true, true, true);
    drop(renderer);

    GIZMO.with(|g| *g.borrow_mut() = Some(controller));

    start_selection_observer();
    start_anim_gizmo_mode_observer();
    Ok(())
}

/// Anchor the gizmo on the effective selection (hide on multi/empty selection).
///
/// In **Scene/Material** mode that's the single outliner selection
/// (`controller().selected`). In **Animation** mode, a selected timeline track
/// takes over: the gizmo snaps to that track's *target node* so the user can
/// pose the bone the track drives (and auto-key it). A track whose target isn't
/// a Transform (light / camera / morph / uniform / builtin) has no gizmo, so the
/// gizmo hides gracefully.
///
/// Combined with `scene.revision` so a selection that fires *before* the bridge
/// entry materializes re-syncs once the entry (and its transform key) appears.
fn start_selection_observer() {
    spawn_local(async move {
        let ctrl = controller();
        let selected_id =
            ctrl.selected
                .signal_ref(|ids| if ids.len() == 1 { Some(ids[0]) } else { None });
        // The effective gizmo anchor: in Animation mode a selected track's
        // transform-target node wins; otherwise the outliner selection.
        map_ref! {
            let scene_id = selected_id,
            let mode = ctrl.mode.signal(),
            let anim_sel = ctrl.anim_selection.signal(),
            let clip = ctrl.current_clip.signal(),
            let _rev = ctrl.scene.revision.signal() => {
                if *mode == EditorMode::Animation && anim_sel.is_some() {
                    anim_selected_transform_node(*clip, *anim_sel)
                } else {
                    *scene_id
                }
            }
        }
        .dedupe()
        .for_each(|id| async move {
            sync_gizmo_selection(id).await;
        })
        .await;
    });
}

/// The scene node a selected animation track drives — `Some(node)` only for a
/// **Transform** track (the only kind with an on-canvas gizmo). Anything else
/// (or no resolvable track) is `None`, so the gizmo hides.
fn anim_selected_transform_node(
    clip: Option<crate::engine::scene::AssetId>,
    sel: Option<crate::controller::animation::AnimSel>,
) -> Option<NodeId> {
    anim_selected_transform(clip, sel).map(|(node, _)| node)
}

/// The (node, channel) a selected Transform track drives, or `None` for a
/// non-Transform track / no resolvable selection.
fn anim_selected_transform(
    clip: Option<crate::engine::scene::AssetId>,
    sel: Option<crate::controller::animation::AnimSel>,
) -> Option<(NodeId, crate::controller::animation::TransformProp)> {
    use crate::controller::animation::{find_clip, TrackTarget};
    let sel = sel?;
    let clip = find_clip(&controller().custom_animations, clip?)?;
    let track = clip.tracks.lock_ref().get(sel.track).cloned()?;
    match track.target {
        TrackTarget::Transform { node, prop } => Some((node, prop)),
        _ => None,
    }
}

/// In Animation mode, match the active gizmo tool to the selected Transform
/// track's channel — Translation→Move, Rotation→Rotate, Scale→Scale — so the
/// gizmo edits the property the track actually animates (and the gizmo commit
/// auto-keys that track). Without this, selecting a rotation track would still
/// show the translate gizmo, and dragging it would move the bone while writing
/// no rotation key. Fires only on selection/clip/mode changes (not per scene
/// revision), so a manual tool switch (Q/W/E/R) sticks until the next track pick.
fn start_anim_gizmo_mode_observer() {
    use crate::controller::animation::TransformProp;
    spawn_local(async move {
        let ctrl = controller();
        map_ref! {
            let mode = ctrl.mode.signal(),
            let sel = ctrl.anim_selection.signal(),
            let clip = ctrl.current_clip.signal() => {
                if *mode == EditorMode::Animation {
                    anim_selected_transform(*clip, *sel).map(|(_, prop)| prop)
                } else {
                    None
                }
            }
        }
        .dedupe()
        .for_each(|prop| async move {
            if let Some(prop) = prop {
                gizmo_mode().set_neq(match prop {
                    TransformProp::Translation => GizmoMode::Move,
                    TransformProp::Rotation => GizmoMode::Rotate,
                    TransformProp::Scale => GizmoMode::Scale,
                });
            }
        })
        .await;
    });
}

/// Whether the gizmo is currently anchored on a target (has a selected object).
///
/// The canvas pointer handler uses this to decide whether to probe for a gizmo
/// grab on pointer-down. In Scene mode the gizmo follows the single outliner
/// selection, but in Animation mode it's anchored to the *selected track's* node
/// with no scene selection — so gating the probe on `controller().selected`
/// alone would leave the gizmo visible but ungrabbable (a drag would fall through
/// to camera orbit). Checking the controller's own `selected_object` covers both.
pub fn has_selection() -> bool {
    GIZMO.with(|g| {
        g.borrow()
            .as_ref()
            .is_some_and(|c| c.selected_object.is_some())
    })
}

/// Look up a scene node's renderer-side transform key via the bridge.
fn transform_key_for_node(id: NodeId) -> Option<TransformKey> {
    bridge()
        .nodes
        .lock()
        .unwrap()
        .get(&id)
        .map(|n| n.transform_key)
}

async fn sync_gizmo_selection(id: Option<NodeId>) {
    let transform_key = id.and_then(transform_key_for_node);

    let handle = renderer_handle();
    let mut renderer = handle.lock().await;
    GIZMO.with(|g| {
        let mut guard = g.borrow_mut();
        let Some(controller) = guard.as_mut() else {
            return;
        };
        let gizmo_enabled = controller_gizmo_enabled();
        match transform_key {
            Some(key) => {
                controller.selected_object = Some(TransformObject {
                    key,
                    instance: None,
                });
                if gizmo_enabled {
                    let (th, rh, sh) = hidden_for_mode(gizmo_mode().get());
                    let _ = controller.set_hidden(&mut renderer, th, rh, sh);
                } else {
                    let _ = controller.set_hidden(&mut renderer, true, true, true);
                }
            }
            None => {
                controller.selected_object = None;
                let _ = controller.set_hidden(&mut renderer, true, true, true);
            }
        }
    });
}

fn controller_gizmo_enabled() -> bool {
    controller().settings.gizmo.get()
}

/// Per-frame update: enforce visibility against the selection + the toggle, keep
/// the gizmo a fixed screen size, and re-anchor it under the selected transform.
/// Called from the render loop after `update_camera`.
pub fn per_frame_update(renderer: &mut AwsmRenderer) {
    GIZMO.with(|g| {
        let mut guard = g.borrow_mut();
        let Some(controller) = guard.as_mut() else {
            return;
        };
        let has_selection = controller.selected_object.is_some();
        let force_hidden = !has_selection || !controller_gizmo_enabled();
        if force_hidden {
            let _ = controller.set_hidden(renderer, true, true, true);
            return;
        }
        // Show only the active tool's handle set (Select shows none).
        let (th, rh, sh) = hidden_for_mode(gizmo_mode().get());
        let _ = controller.set_hidden(renderer, th, rh, sh);
        let Some(matrices) = renderer.camera.last_matrices.as_ref().cloned() else {
            return;
        };
        let _ = controller.zoom_gizmo_transforms(renderer, &matrices);
    });
}

/// Try to grab a gizmo handle at screen `(x, y)` via analytic picking. Returns
/// `true` when a handle was grabbed (the caller then routes pointer-move to the
/// gizmo instead of the camera). The renderer lock is held by the caller.
pub fn try_start_pick(renderer: &mut AwsmRenderer, x: i32, y: i32) -> bool {
    GIZMO.with(|g| {
        let mut guard = g.borrow_mut();
        let Some(controller) = guard.as_mut() else {
            return false;
        };
        controller.try_grab(renderer, x, y).is_some()
    })
}

/// Update the hovered-handle highlight from a screen position (call on
/// pointer-move when NOT dragging). Renderer-free — picks against the gizmo's
/// cached placement — so it's cheap to call on every move.
pub fn update_hover(x: i32, y: i32) {
    GIZMO.with(|g| {
        if let Some(controller) = g.borrow_mut().as_mut() {
            controller.update_hover(x, y);
        }
    });
}

/// Clear any hover highlight (call when the pointer leaves the canvas).
pub fn clear_hover() {
    GIZMO.with(|g| {
        if let Some(controller) = g.borrow_mut().as_mut() {
            controller.clear_hover();
        }
    });
}

/// Apply a pointer-move delta to the in-flight gizmo drag.
pub fn drag(renderer: &mut AwsmRenderer, dx: i32, dy: i32) {
    GIZMO.with(|g| {
        if let Some(controller) = g.borrow_mut().as_mut() {
            controller.update_transform(renderer, dx, dy);
        }
    });
}

/// Clear the controller's in-flight drag state + active highlight. Called on
/// pointer-up alongside `commit_drag`.
pub fn end_drag() {
    GIZMO.with(|g| {
        if let Some(controller) = g.borrow_mut().as_mut() {
            controller.end_drag();
        }
    });
}

/// Map a transform key back to a scene node id (checks the node's primary
/// transform and any populated sub-transforms).
fn node_for_transform_key(transform_key: TransformKey) -> Option<NodeId> {
    let bridge = bridge();
    let nodes = bridge.nodes.lock().unwrap();
    for (id, entry) in nodes.iter() {
        if entry.transform_key == transform_key {
            return Some(*id);
        }
        if entry
            .model_transforms
            .lock()
            .unwrap()
            .contains(&transform_key)
        {
            return Some(*id);
        }
    }
    None
}

thread_local! {
    /// The selected node + its transform at the start of a gizmo drag, so the
    /// pointer-up commit can record the correct undo inverse.
    static DRAG_START: RefCell<Option<(NodeId, crate::engine::scene::types::Trs)>> =
        const { RefCell::new(None) };
}

/// Capture the selected node's transform at the start of a gizmo drag.
pub fn begin_drag() {
    let selected = GIZMO.with(|g| g.borrow().as_ref().and_then(|c| c.selected_object));
    let Some(sel) = selected else {
        return;
    };
    let Some(node_id) = node_for_transform_key(sel.key) else {
        return;
    };
    let trs = bridge()
        .nodes
        .lock()
        .unwrap()
        .get(&node_id)
        .map(|n| n.node.transform.get());
    if let Some(trs) = trs {
        DRAG_START.with(|d| *d.borrow_mut() = Some((node_id, trs)));
    }
}

/// On gizmo-drag release: dispatch the net move as a single `SetTransform`
/// command so it flows through the `EditorController` (undoable + MCP-drivable).
/// Reads the renderer's final local transform, resets the node to the drag-start
/// value (so the controller captures the correct inverse), then dispatches the
/// final value.
pub fn commit_drag() {
    // Clear the controller's in-flight drag + active-handle highlight (covers
    // both pointer-up and pointer-cancel, which both route here).
    end_drag();
    let start = DRAG_START.with(|d| d.borrow_mut().take());
    let selected = GIZMO.with(|g| g.borrow().as_ref().and_then(|c| c.selected_object));
    let (Some((node_id, start_trs)), Some(sel)) = (start, selected) else {
        // No captured drag — fall back to the plain live sync.
        sync_scene_transform_from_renderer();
        return;
    };
    spawn_local(async move {
        let local = {
            let handle = renderer_handle();
            let renderer = handle.lock().await;
            renderer.transforms.get_local(sel.key).cloned().ok()
        };
        let Some(local) = local else {
            return;
        };
        let final_trs = crate::engine::scene::types::Trs {
            translation: local.translation.to_array(),
            rotation: local.rotation.to_array(),
            scale: local.scale.to_array(),
        };
        if final_trs == start_trs {
            return;
        }
        // Reset the node to the start value so the controller records start→final.
        if let Some(node) = bridge()
            .nodes
            .lock()
            .unwrap()
            .get(&node_id)
            .map(|n| n.node.clone())
        {
            node.transform.set(start_trs);
        }
        let _ = controller()
            .dispatch(EditorCommand::SetTransform {
                id: node_id,
                transform: final_trs,
            })
            .await;
        // AUTO-KEY (DCC-style): in Animation mode with the Settings toggle on,
        // a gizmo commit on a node the current clip tracks writes keyframe(s)
        // at the playhead straight from the committed pose — one undo entry per
        // key, after the transform's own entry. Gesture-level on purpose: only
        // a REAL gizmo drag auto-keys (programmatic SetTransform — MCP, IK
        // apply, undo replay — never does).
        auto_key_from_commit(node_id, final_trs).await;
    });
}

/// Write keyframe(s) for a committed gizmo pose when auto-key applies: every
/// Transform track of the CURRENT clip that targets `node_id` gets a key at
/// the playhead with the matching component of `trs`.
async fn auto_key_from_commit(node_id: NodeId, trs: crate::engine::scene::types::Trs) {
    use crate::controller::animation::{find_clip, TrackTarget, TrackValue, TransformProp};
    use crate::controller::EditorCommand;
    let ctrl = controller();
    if !ctrl.settings.auto_key.get() || ctrl.mode.get() != EditorMode::Animation {
        return;
    }
    let Some(clip_id) = ctrl.current_clip.get() else {
        return;
    };
    let Some(clip) = find_clip(&ctrl.custom_animations, clip_id) else {
        return;
    };
    let t = ctrl.playhead.get();
    let keys: Vec<(usize, TrackValue)> = clip
        .tracks
        .lock_ref()
        .iter()
        .enumerate()
        .filter_map(|(i, tr)| {
            let TrackTarget::Transform { node, prop } = &tr.target else {
                return None;
            };
            if *node != node_id {
                return None;
            }
            Some((
                i,
                match prop {
                    TransformProp::Translation => TrackValue::Vec3(trs.translation),
                    TransformProp::Rotation => TrackValue::Quat(trs.rotation),
                    TransformProp::Scale => TrackValue::Vec3(trs.scale),
                },
            ))
        })
        .collect();
    for (track, value) in keys {
        let _ = ctrl
            .dispatch(EditorCommand::AddKeyframe {
                clip: clip_id,
                track,
                t,
                value,
                interp: None,
            })
            .await;
    }
}

/// Write the dragged node's renderer-side local transform back into its scene
/// `Trs` so the Inspector updates live. Spawns its own renderer read.
pub fn sync_scene_transform_from_renderer() {
    let selected = GIZMO.with(|g| g.borrow().as_ref().and_then(|c| c.selected_object));
    let Some(selected) = selected else {
        return;
    };
    spawn_local(async move {
        let handle = renderer_handle();
        let local = {
            let renderer = handle.lock().await;
            renderer.transforms.get_local(selected.key).cloned().ok()
        };
        let Some(local) = local else {
            return;
        };
        let Some(node_id) = node_for_transform_key(selected.key) else {
            return;
        };
        let node = bridge()
            .nodes
            .lock()
            .unwrap()
            .get(&node_id)
            .map(|n| n.node.clone());
        if let Some(node) = node {
            node.transform.set(crate::engine::scene::types::Trs {
                translation: local.translation.to_array(),
                rotation: local.rotation.to_array(),
                scale: local.scale.to_array(),
            });
        }
    });
}
