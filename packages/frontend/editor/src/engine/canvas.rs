//! The WebGPU canvas. M3 wires camera navigation (orbit / pan / zoom) only;
//! GPU picking + the gizmo drag land with the viewport chrome (M6).

use awsm_web_shared::prelude::*;

use super::context::{try_with_camera_mut, with_camera_mut};

pub fn render_canvas(on_ready: impl FnOnce(web_sys::HtmlCanvasElement) + 'static) -> Dom {
    html!("canvas" => web_sys::HtmlCanvasElement, {
        .style("width", "100%")
        .style("height", "100%")
        .style("display", "block")
        .style("touch-action", "none")
        .after_inserted(on_ready)
        .with_node!(canvas => {
            .event(clone!(canvas => move |event: events::PointerDown| {
                if event.button() != events::MouseButton::Left {
                    return;
                }
                let _ = canvas.set_pointer_capture(event.pointer_id());
                // Camera-orbit start (picking/gizmo selection lands in M6).
                with_camera_mut(|c| c.on_pointer_down());
            }))
            .event(move |event: events::PointerMove| {
                let dx = event.movement_x();
                let dy = event.movement_y();
                if dx == 0 && dy == 0 {
                    return;
                }
                let panning = event.shift_key() || event.alt_key();
                try_with_camera_mut(|c| c.on_pointer_move(dx, dy, panning));
            })
            .event(move |_: events::PointerUp| { try_with_camera_mut(|c| c.on_pointer_up()); })
            .event(move |_: events::PointerCancel| { try_with_camera_mut(|c| c.on_pointer_up()); })
            .event(move |event: events::Wheel| {
                event.prevent_default();
                // Mounted before create_context resolves — silently drop early
                // scrolls rather than panicking.
                try_with_camera_mut(|c| c.on_wheel(event.delta_y()));
            })
        })
    })
}
