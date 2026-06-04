//! The WebGPU canvas. Pointer handling distinguishes a **drag** (camera orbit /
//! pan) from a **click** (a GPU pick → select the hit node, or deselect on a
//! miss). The transform gizmo drag layers in next (M6).

use std::cell::Cell;
use std::rc::Rc;

use awsm_renderer::picker::PickResult;
use awsm_web_shared::prelude::*;
use gloo_timers::future::TimeoutFuture;
use wasm_bindgen_futures::spawn_local;

use super::context::{renderer_handle, try_with_camera_mut, with_camera_mut};
use crate::controller::{controller, EditorCommand};
use crate::engine::bridge::bridge;

/// Pixels of movement before a pointer-down is treated as a drag (not a click).
const DRAG_THRESHOLD: f64 = 4.0;

pub fn render_canvas(on_ready: impl FnOnce(web_sys::HtmlCanvasElement) + 'static) -> Dom {
    // (down position, moved-past-threshold). `None` = no pointer down.
    let drag: Rc<Cell<Option<(f64, f64, bool)>>> = Rc::new(Cell::new(None));

    html!("canvas" => web_sys::HtmlCanvasElement, {
        .style("width", "100%")
        .style("height", "100%")
        .style("display", "block")
        .style("touch-action", "none")
        .after_inserted(on_ready)
        .with_node!(canvas => {
            .event(clone!(canvas, drag => move |event: events::PointerDown| {
                if event.button() != events::MouseButton::Left {
                    return;
                }
                let _ = canvas.set_pointer_capture(event.pointer_id());
                drag.set(Some((event.x(), event.y(), false)));
                with_camera_mut(|c| c.on_pointer_down());
            }))
            .event(clone!(drag => move |event: events::PointerMove| {
                let dx = event.movement_x();
                let dy = event.movement_y();
                if dx == 0 && dy == 0 {
                    return;
                }
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
            }))
            .event(clone!(canvas, drag => move |event: events::PointerUp| {
                try_with_camera_mut(|c| c.on_pointer_up());
                let was = drag.replace(None);
                // A click (no significant drag) runs a GPU pick → select.
                if let Some((_, _, false)) = was {
                    let rect = canvas.get_bounding_client_rect();
                    let lx = (event.x() - rect.left()) as i32;
                    let ly = (event.y() - rect.top()) as i32;
                    pick_and_select(lx, ly);
                }
            }))
            .event(clone!(drag => move |_: events::PointerCancel| {
                try_with_camera_mut(|c| c.on_pointer_up());
                drag.set(None);
            }))
            .event(move |event: events::Wheel| {
                event.prevent_default();
                try_with_camera_mut(|c| c.on_wheel(event.delta_y()));
            })
        })
    })
}

/// Run a GPU pick at canvas-local `(x, y)` and dispatch the resulting selection.
fn pick_and_select(x: i32, y: i32) {
    spawn_local(async move {
        let handle = renderer_handle();
        // The GPU picker subsystem compiles lazily on first use — `pick()`
        // returns `Initializing` until the pipeline/bind-group are ready (and
        // `InFlight` while a prior pick drains). Retry across a few frames so the
        // user's *first* viewport click selects rather than silently no-opping.
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
                if let Some(node_id) = bridge().node_for_mesh(mesh_key) {
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
