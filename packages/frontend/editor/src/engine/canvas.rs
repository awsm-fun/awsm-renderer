//! The WebGPU canvas. Pointer handling distinguishes a **gizmo drag** (a handle
//! grab → translate/rotate/scale the selection), a **camera drag** (orbit / pan),
//! and a **click** (a GPU pick → select the hit node, or deselect on a miss).
//!
//! On press, if a single node is selected with the gizmo enabled, we GPU-pick to
//! see whether a gizmo handle was grabbed; that decision (async, ~1 frame) routes
//! the gesture to the gizmo or the camera. Otherwise the camera starts
//! immediately (the fast path for empty-space / no-selection navigation).

use std::cell::Cell;
use std::rc::Rc;

use awsm_renderer::picker::PickResult;
use awsm_web_shared::prelude::*;
use gloo_timers::future::TimeoutFuture;
use wasm_bindgen_futures::spawn_local;

use super::context::{renderer_handle, try_with_camera_mut, with_camera_mut};
use super::{curve_handles, gizmo};
use crate::controller::{controller, EditorCommand};
use crate::engine::bridge::bridge;

/// Pixels of movement before a pointer-down is treated as a drag (not a click).
const DRAG_THRESHOLD: f64 = 4.0;

/// Whether the viewport is locked to a scene `Camera` node (vs the free editor
/// camera). When true, orbit / pan / zoom are suppressed.
fn scene_camera_active() -> bool {
    controller().active_camera.get().is_some()
}

/// Which kind of pointer drag won the press. `None` while the gizmo-vs-camera
/// pick is still in flight.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MoveAction {
    Camera,
    Gizmo,
    /// Dragging a curve control-point handle (translate one point in the plane).
    CurveHandle,
}

/// Canvas-local coordinates for a client point — the space the GPU picker +
/// gizmo ray expect. The renderer configures its WebGPU surface in CSS pixels,
/// so this is a plain rect-relative offset (matching the selection pick path);
/// no device-pixel scaling.
fn canvas_coords(canvas: &web_sys::HtmlCanvasElement, client_x: f64, client_y: f64) -> (i32, i32) {
    let rect = canvas.get_bounding_client_rect();
    (
        (client_x - rect.left()) as i32,
        (client_y - rect.top()) as i32,
    )
}

pub fn render_canvas(on_ready: impl FnOnce(web_sys::HtmlCanvasElement) + 'static) -> Dom {
    // (down position, moved-past-threshold). `None` = no pointer down.
    let drag: Rc<Cell<Option<(f64, f64, bool)>>> = Rc::new(Cell::new(None));
    // Which drag won this press (None until the gizmo pick resolves).
    let action: Rc<Cell<Option<MoveAction>>> = Rc::new(Cell::new(None));

    html!("canvas" => web_sys::HtmlCanvasElement, {
        .style("width", "100%")
        .style("height", "100%")
        .style("display", "block")
        .style("touch-action", "none")
        .after_inserted(on_ready)
        .with_node!(canvas => {
            .event(clone!(canvas, drag, action => move |event: events::PointerDown| {
                if event.button() != events::MouseButton::Left {
                    return;
                }
                let _ = canvas.set_pointer_capture(event.pointer_id());
                drag.set(Some((event.x(), event.y(), false)));

                // Only probe for a handle/gizmo grab when one is actually on
                // screen — a single selection with the gizmo enabled, or a
                // selected curve (whose control-point handles show regardless of
                // the gizmo toggle). Otherwise start the camera immediately so
                // navigation stays crisp.
                //
                // The gizmo is also grabbable when it's anchored *without* a scene
                // selection — in Animation mode it follows the selected track's
                // node (`controller().selected` is empty there), so gating purely
                // on `single` would render the gizmo visible but un-draggable and
                // every drag would orbit the camera instead of posing the bone.
                let single = controller().selected.lock_ref().len() == 1;
                let gizmo_on = controller().settings.gizmo.get();
                let gizmo_probe = gizmo_on && (single || gizmo::has_selection());
                let probe = gizmo_probe || (single && curve_handles::has_active_handles());
                if !probe {
                    // A scene camera locks the view — don't start an orbit/pan;
                    // leaving `action` unset still lets a click pick + select.
                    if !scene_camera_active() {
                        action.set(Some(MoveAction::Camera));
                        with_camera_mut(|c| c.on_pointer_down());
                    }
                    return;
                }

                action.set(None);
                let (px, py) = canvas_coords(&canvas, event.x(), event.y());
                spawn_local(clone!(action => async move {
                    let handle = renderer_handle();
                    // 1) The gizmo is picked ANALYTICALLY (a CPU ray-cast with a
                    //    screen-space tolerance band) and takes priority — it
                    //    needs no GPU pick, so it's checked first and synchronously
                    //    under one lock.
                    if gizmo_on {
                        let grabbed = {
                            let mut r = handle.lock().await;
                            gizmo::try_start_pick(&mut r, px, py)
                        };
                        if grabbed {
                            action.set(Some(MoveAction::Gizmo));
                            gizmo::begin_drag();
                            return;
                        }
                    }
                    // 2) Otherwise GPU-pick for curve control-point handles. Retry
                    //    across a few frames while the picker (re)compiles; release
                    //    the lock between attempts so the render loop keeps running.
                    //    `None` falls through to the camera (object selection
                    //    happens on click-up, not here).
                    let mut resolved: Option<Option<MoveAction>> = None;
                    for attempt in 0..12 {
                        let res: Option<Option<MoveAction>> = {
                            let mut r = handle.lock().await;
                            match r.pick(px, py).await {
                                Ok(PickResult::Hit(mesh_key)) => {
                                    if curve_handles::try_start_pick(&mut r, Some(mesh_key), px, py) {
                                        Some(Some(MoveAction::CurveHandle))
                                    } else {
                                        Some(None)
                                    }
                                }
                                // Empty-space hit: still allow a near-miss grab of
                                // a small control-point handle (CPU tolerance).
                                Ok(PickResult::Miss) => {
                                    if curve_handles::try_start_pick(&mut r, None, px, py) {
                                        Some(Some(MoveAction::CurveHandle))
                                    } else {
                                        Some(None)
                                    }
                                }
                                Ok(PickResult::Initializing) | Ok(PickResult::InFlight) => None,
                                _ => Some(None),
                            }
                        };
                        if let Some(r) = res {
                            resolved = Some(r);
                            break;
                        }
                        if attempt < 11 {
                            TimeoutFuture::new(16).await;
                        }
                    }
                    match resolved.flatten() {
                        Some(MoveAction::CurveHandle) => {
                            action.set(Some(MoveAction::CurveHandle));
                        }
                        _ if !scene_camera_active() => {
                            action.set(Some(MoveAction::Camera));
                            with_camera_mut(|c| c.on_pointer_down());
                        }
                        _ => {}
                    }
                }));
            }))
            .event(clone!(canvas, drag, action => move |event: events::PointerMove| {
                // Idle hover (no button down, no pending pick): highlight the
                // gizmo handle under the cursor. Done on every move from the
                // absolute position — NOT gated on movement deltas (synthetic
                // moves and some real ones report movementX/Y == 0). Renderer-free.
                if action.get().is_none() && drag.get().is_none() {
                    let (px, py) = canvas_coords(&canvas, event.x(), event.y());
                    gizmo::update_hover(px, py);
                    return;
                }
                let dx = event.movement_x();
                let dy = event.movement_y();
                if dx == 0 && dy == 0 {
                    return;
                }
                match action.get() {
                    Some(MoveAction::Gizmo) => {
                        spawn_local(async move {
                            {
                                let handle = renderer_handle();
                                let mut r = handle.lock().await;
                                gizmo::drag(&mut r, dx, dy);
                            }
                            gizmo::sync_scene_transform_from_renderer();
                        });
                    }
                    Some(MoveAction::CurveHandle) => {
                        spawn_local(async move {
                            let handle = renderer_handle();
                            let mut r = handle.lock().await;
                            // Moves the handle + writes the control point back into
                            // the node kind (polyline + Inspector follow live).
                            curve_handles::drag(&mut r, dx, dy);
                        });
                    }
                    Some(MoveAction::Camera) => {
                        if let Some((sx, sy, moved)) = drag.get() {
                            let moved = moved
                                || (event.x() - sx).abs() > DRAG_THRESHOLD
                                || (event.y() - sy).abs() > DRAG_THRESHOLD;
                            drag.set(Some((sx, sy, moved)));
                            if moved {
                                let panning = event.shift_key() || event.alt_key();
                                try_with_camera_mut(|c| c.on_pointer_move(dx, dy, panning));
                            }
                        }
                    }
                    // Button down, gizmo pick still pending — remember it became
                    // a drag. (Idle hover is handled at the top of this handler.)
                    None => {
                        if let Some((sx, sy, moved)) = drag.get() {
                            let moved = moved
                                || (event.x() - sx).abs() > DRAG_THRESHOLD
                                || (event.y() - sy).abs() > DRAG_THRESHOLD;
                            drag.set(Some((sx, sy, moved)));
                        }
                    }
                }
            }))
            // Clear the hover highlight when the pointer leaves the canvas.
            .event(|_: events::PointerLeave| gizmo::clear_hover())
            .event(clone!(canvas, drag, action => move |event: events::PointerUp| {
                try_with_camera_mut(|c| c.on_pointer_up());
                let finished = action.get();
                action.set(None);
                let was = drag.replace(None);
                if finished == Some(MoveAction::Gizmo) {
                    // Commit the net move as a SetTransform command (undoable +
                    // MCP-drivable); lands the Inspector on the exact final value.
                    gizmo::commit_drag();
                } else if finished == Some(MoveAction::CurveHandle) {
                    // Commit the moved control point as one undoable SetKind.
                    curve_handles::commit_drag();
                } else if let Some((_, _, false)) = was {
                    // A click (no significant drag) runs a GPU pick → select.
                    let (px, py) = canvas_coords(&canvas, event.x(), event.y());
                    pick_and_select(px, py);
                }
            }))
            .event(clone!(drag, action => move |_: events::PointerCancel| {
                try_with_camera_mut(|c| c.on_pointer_up());
                // Commit any in-flight gizmo / handle drag so an interrupted
                // gesture still lands as one undoable command (no scene edit
                // escapes the EditorController), matching pointer-up.
                match action.get() {
                    Some(MoveAction::Gizmo) => gizmo::commit_drag(),
                    Some(MoveAction::CurveHandle) => curve_handles::commit_drag(),
                    _ => {}
                }
                action.set(None);
                drag.set(None);
            }))
            .event(move |event: events::Wheel| {
                event.prevent_default();
                // A scene camera locks zoom too.
                if !scene_camera_active() {
                    try_with_camera_mut(|c| c.on_wheel(event.delta_y()));
                }
            })
        })
    })
}

/// Compile the GPU picker subsystem in the background shortly after boot, so the
/// user's *first* viewport click selects immediately. The picker compiles lazily
/// on the first `pick()` and its id-attachment only carries hits on a frame
/// rendered *after* that compile — so a cold first click would otherwise read an
/// empty id-buffer and miss. A few throwaway picks across early frames force the
/// compile + prime the attachment before any real click arrives.
pub fn prewarm_picker() {
    spawn_local(async move {
        // Let a couple of frames render first so the renderer is settled.
        TimeoutFuture::new(150).await;
        let handle = renderer_handle();
        for _ in 0..3 {
            {
                let mut r = handle.lock().await;
                let _ = r.pick(0, 0).await;
            }
            TimeoutFuture::new(32).await;
        }
    });
}

/// Run a GPU pick at canvas-local `(x, y)` and dispatch the resulting selection.
fn pick_and_select(x: i32, y: i32) {
    spawn_local(async move {
        let handle = renderer_handle();
        // The GPU picker compiles lazily on first use — `pick()` returns
        // `Initializing` until the pipeline/bind-group are ready (and `InFlight`
        // while a prior pick drains). Retry across a few frames so the user's
        // first click selects rather than silently no-opping. (The picker is also
        // pre-warmed at boot; see `prewarm_picker`.)
        let mut result = Ok(PickResult::Initializing);
        for attempt in 0..12 {
            result = {
                let mut r = handle.lock().await;
                r.pick(x, y).await
            };
            match result {
                Ok(PickResult::Initializing) | Ok(PickResult::InFlight) => {
                    if attempt < 11 {
                        TimeoutFuture::new(16).await;
                    }
                }
                _ => break,
            }
        }
        match result {
            Ok(PickResult::Hit(mesh_key)) => {
                // A light has no scene mesh — clicking its HUD icon resolves to
                // the light node. Check that first, then the normal mesh → node
                // lookup.
                let node_id = {
                    let r = handle.lock().await;
                    super::light_icons::try_pick(&r, mesh_key, x, y)
                }
                .or_else(|| bridge().node_for_mesh(mesh_key));
                if let Some(node_id) = node_id {
                    let _ = controller()
                        .dispatch(EditorCommand::SetSelection { ids: vec![node_id] })
                        .await;
                }
            }
            Ok(PickResult::Miss) => {
                let _ = controller()
                    .dispatch(EditorCommand::SetSelection { ids: vec![] })
                    .await;
            }
            // Initializing / InFlight / Disabled — leave the selection alone.
            _ => {}
        }
    });
}
