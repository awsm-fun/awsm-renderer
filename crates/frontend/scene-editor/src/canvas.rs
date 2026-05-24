//! The WebGPU canvas.
//!
//! Pointer events on the canvas are routed three ways:
//! 1. Pointerdown triggers a GPU pick; if a gizmo-handle mesh is hit we
//!    start a gizmo drag. If another mesh is hit we select that scene
//!    node. Otherwise we start an orbit-camera drag.
//! 2. Pointermove routes to gizmo-drag or camera-orbit depending on the
//!    current `move_action`.
//! 3. Pointerup finalises whichever drag was live.

use crate::context::{renderer_handle, try_with_camera_mut, with_camera_mut};
use crate::prelude::*;
use crate::renderer_bridge::gizmo::{self, MoveAction};
use crate::renderer_bridge::point_handle_sync;
use crate::state::app_state;
use awsm_renderer::picker::PickResult;
use awsm_renderer_editor::transform_controller::TransformTarget;
use wasm_bindgen_futures::spawn_local;

pub fn render_canvas(on_ready: impl FnOnce(web_sys::HtmlCanvasElement) + 'static) -> Dom {
    static CONTAINER: LazyLock<String> = LazyLock::new(|| {
        class! {
            .style("width", "100%")
            .style("height", "100%")
            .style("display", "block")
            .style("touch-action", "none")
        }
    });

    html!("canvas" => web_sys::HtmlCanvasElement, {
        .class([&*CONTAINER, &*USER_SELECT_NONE])
        .after_inserted(|canvas| {
            on_ready(canvas);
        })
        .with_node!(canvas => {
            .event(clone!(canvas => move |event: events::PointerDown| {
                if event.button() != events::MouseButton::Left {
                    return;
                }
                let _ = canvas.set_pointer_capture(event.pointer_id());
                let pointer_x = event.x() as i32;
                let pointer_y = event.y() as i32;
                on_pointer_down(&canvas, pointer_x, pointer_y, event.shift_key());
            }))
            .event(move |event: events::PointerMove| {
                let dx = event.movement_x();
                let dy = event.movement_y();
                if dx == 0 && dy == 0 {
                    return;
                }
                let panning = event.shift_key() || event.alt_key();
                on_pointer_move(dx, dy, panning);
            })
            .event(move |_: events::PointerUp| {
                on_pointer_up();
            })
            .event(move |_: events::PointerCancel| {
                on_pointer_up();
            })
            .event(move |event: events::Wheel| {
                event.prevent_default();
                // `render_canvas` mounts before `create_context` resolves.
                // A wheel scroll during that race window would otherwise
                // panic the wasm — silently drop instead, the user will
                // scroll again once the editor's actually ready.
                try_with_camera_mut(|c| c.on_wheel(event.delta_y()));
            })
        })
    })
}

fn on_pointer_down(canvas: &web_sys::HtmlCanvasElement, x: i32, y: i32, _shift: bool) {
    // Translate CSS pointer coords → canvas backing-store coords by
    // snapping to the ratio the renderer actually drew at. For our DPR
    // of 1 it's a no-op.
    let rect = canvas.get_bounding_client_rect();
    let scale_x = canvas.width() as f64 / rect.width().max(1.0);
    let scale_y = canvas.height() as f64 / rect.height().max(1.0);
    let local_x = ((x as f64 - rect.left()) * scale_x) as i32;
    let local_y = ((y as f64 - rect.top()) * scale_y) as i32;

    spawn_local(async move {
        let state = app_state();

        // Run the GPU pick to see what's under the cursor.
        let handle = renderer_handle();
        let pick_result = {
            let renderer = handle.lock().await;
            renderer.pick(local_x, local_y).await
        };

        let mut started_gizmo = false;

        match pick_result {
            Ok(PickResult::Hit(mesh_key)) => {
                let mut renderer = handle.lock().await;
                // Point-handle gizmo takes priority over TRS gizmo + scene
                // object selection. Drag math projects against the
                // camera-facing plane through the handle's anchor.
                let point_handle_hit = {
                    let mut handles = state.point_handles.lock().unwrap();
                    if let Some(idx) = handles.is_handle_mesh(mesh_key) {
                        handles.start_drag(&renderer, idx, local_x, local_y);
                        Some(idx)
                    } else {
                        None
                    }
                };
                if point_handle_hit.is_some() {
                    started_gizmo = true;
                    state.move_action.set(Some(MoveAction::PointHandleDragging));
                    *state.pending_transform_snapshot.lock().unwrap() =
                        Some(state.snapshot_scene());
                    drop(renderer);
                } else {
                    let mut controller_lock = state.transform_controller.lock().unwrap();
                    if let Some(controller) = controller_lock.as_mut() {
                        match controller.start_pick(&mut renderer, mesh_key, local_x, local_y) {
                            Some(TransformTarget::GizmoHit(_)) => {
                                started_gizmo = true;
                                state.move_action.set(Some(MoveAction::GizmoTransforming));
                                *state.pending_transform_snapshot.lock().unwrap() =
                                    Some(state.snapshot_scene());
                            }
                            Some(TransformTarget::ObjectHit(obj)) => {
                                drop(controller_lock);
                                drop(renderer);
                                if let Some(node_id) = gizmo::node_for_transform_key(obj.key) {
                                    state.select_only(node_id);
                                }
                            }
                            None => {}
                        }
                    }
                }
            }
            // Click landed on empty space — clear the current selection.
            // (`Initializing` / `InFlight` aren't user-meaningful misses;
            // the pick infra wasn't ready, so leave selection alone.)
            Ok(PickResult::Miss) => {
                // Before clearing, walk point-handles in a small pixel
                // tolerance around the cursor. Handles render small on
                // screen (8px radius); when two project close together
                // the per-pixel GPU pick can miss the intended target.
                // This is a CPU-side fallback that projects each
                // visible handle and picks the closest within 6px.
                let renderer = handle.lock().await;
                let tolerance_hit = {
                    let mut handles = state.point_handles.lock().unwrap();
                    let idx = handles.pick_with_tolerance(&renderer, local_x, local_y, 6.0);
                    if let Some(idx) = idx {
                        handles.start_drag(&renderer, idx, local_x, local_y);
                    }
                    idx
                };
                if tolerance_hit.is_some() {
                    started_gizmo = true;
                    state.move_action.set(Some(MoveAction::PointHandleDragging));
                    *state.pending_transform_snapshot.lock().unwrap() =
                        Some(state.snapshot_scene());
                    drop(renderer);
                } else {
                    drop(renderer);
                    state.clear_selection();
                }
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!("pick error: {:?}", err);
            }
        }

        if !started_gizmo {
            // Tolerate the same init race as the wheel handler — a
            // pointer-down before `create_context` resolves is a no-op
            // (no camera to start orbiting yet).
            if try_with_camera_mut(|c| c.on_pointer_down()).is_some() {
                state.move_action.set(Some(MoveAction::CameraMoving));
            }
        }
    });
}

fn on_pointer_move(dx: i32, dy: i32, panning: bool) {
    let state = app_state();
    match state.move_action.get() {
        Some(MoveAction::GizmoTransforming) => {
            spawn_local(async move {
                let handle = renderer_handle();
                let mut renderer = handle.lock().await;
                let state = app_state();
                {
                    let mut controller_lock = state.transform_controller.lock().unwrap();
                    if let Some(controller) = controller_lock.as_mut() {
                        controller.update_transform(&mut renderer, dx, dy);
                    }
                }
                drop(renderer);
                gizmo::sync_scene_transform_from_renderer();
            });
        }
        Some(MoveAction::PointHandleDragging) => {
            spawn_local(async move {
                let handle = renderer_handle();
                let mut renderer = handle.lock().await;
                let state = app_state();
                let drag_result = {
                    let mut handles = state.point_handles.lock().unwrap();
                    handles.update_drag(&mut renderer, dx, dy)
                };
                drop(renderer);
                if let (Some(target), Some((idx, world_pos))) =
                    (state.point_handle_target.get(), drag_result)
                {
                    point_handle_sync::apply_drag(target, idx, world_pos).await;
                }
            });
        }
        Some(MoveAction::CameraMoving) => {
            // CameraMoving is only set after `on_pointer_down`
            // confirmed AppContext is ready, so `with_camera_mut`
            // here can stay panicking — if it ever fires before
            // init that's a real bug (a CameraMoving move_action
            // somehow set without a successful pointer_down).
            with_camera_mut(|c| c.on_pointer_move(dx, dy, panning));
        }
        None => {}
    }
}

fn on_pointer_up() {
    let state = app_state();
    let prev_action = state.move_action.get();
    let was_gizmo = matches!(
        prev_action,
        Some(MoveAction::GizmoTransforming) | Some(MoveAction::PointHandleDragging)
    );
    state.move_action.set(None);

    if matches!(prev_action, Some(MoveAction::PointHandleDragging)) {
        state.point_handles.lock().unwrap().end_drag();
    }

    // PointerUp can fire from outside the canvas if a pointer-down
    // raced ahead of `create_context`. Same race as the wheel
    // handler — tolerate, don't panic.
    try_with_camera_mut(|c| c.on_pointer_up());

    if was_gizmo {
        // Commit the drag as a single history entry.
        let snapshot = state.pending_transform_snapshot.lock().unwrap().take();
        if let Some(previous) = snapshot {
            state.scene.bump_revision();
            state.commit_history(previous);
        }
    }
}
