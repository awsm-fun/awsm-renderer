//! Shared canvas-sizing + resize-forwarding for the worker-hosted demos
//! (`docs/plans/multithreading.md` H1).
//!
//! The renderer already re-creates its size-dependent render textures whenever
//! the swap-chain `getCurrentTexture().getSize()` changes — and for an
//! `OffscreenCanvas` that size *is* the canvas backing store. So getting crisp,
//! correctly-aspected output is purely about (a) sizing the canvas backing
//! store to **CSS size × devicePixelRatio** before transfer, (b) forwarding
//! live resizes to the worker, and (c) reading the camera aspect from the live
//! canvas size instead of a hard-coded constant.

use wasm_bindgen::prelude::*;
use web_sys::js_sys;

/// Main thread: set the canvas backing store to CSS size × `devicePixelRatio`
/// so the worker renders at native resolution (no upscaling). Call **before**
/// `transfer_control_to_offscreen()`. Returns the backing size.
pub fn size_canvas_to_display(canvas: &web_sys::HtmlCanvasElement) -> (u32, u32) {
    let dpr = web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0)
        .max(1.0);
    let w = (((canvas.client_width().max(1)) as f64) * dpr).round() as u32;
    let h = (((canvas.client_height().max(1)) as f64) * dpr).round() as u32;
    let (w, h) = (w.max(1), h.max(1));
    canvas.set_width(w);
    canvas.set_height(h);
    (w, h)
}

/// Main thread: post `{ kind: "resize", w, h }` (CSS size × dpr) to `worker`
/// whenever the canvas's CSS size **or** the devicePixelRatio changes. Call
/// after the canvas is transferred.
///
/// A `ResizeObserver` catches CSS-box changes; a re-arming `matchMedia`
/// resolution query catches pure DPR changes (e.g. dragging the window between
/// monitors with different scaling) — those leave the CSS box unchanged, so the
/// observer alone would miss them and the canvas would render blurry.
pub fn observe_resize(
    canvas: &web_sys::HtmlCanvasElement,
    worker: &web_sys::Worker,
) -> Result<(), JsValue> {
    let send: std::rc::Rc<dyn Fn()> = {
        let worker = worker.clone();
        let canvas = canvas.clone();
        std::rc::Rc::new(move || {
            let dpr = web_sys::window()
                .map(|w| w.device_pixel_ratio())
                .unwrap_or(1.0)
                .max(1.0);
            let w = (((canvas.client_width().max(1)) as f64) * dpr).round() as u32;
            let h = (((canvas.client_height().max(1)) as f64) * dpr).round() as u32;
            let msg = js_sys::Object::new();
            set(&msg, "kind", &JsValue::from_str("resize"));
            set(&msg, "w", &JsValue::from_f64(w.max(1) as f64));
            set(&msg, "h", &JsValue::from_f64(h.max(1) as f64));
            let _ = worker.post_message(&msg);
        })
    };

    // CSS-box changes.
    let send_ro = send.clone();
    let cb = Closure::<dyn FnMut(js_sys::Array)>::new(move |_entries: js_sys::Array| send_ro());
    let observer = web_sys::ResizeObserver::new(cb.as_ref().unchecked_ref())?;
    observer.observe(canvas);
    cb.forget();
    std::mem::forget(observer);

    // DPR changes (self-re-arming for the new ratio).
    arm_dpr_watch(send.clone());
    Ok(())
}

/// Re-arming `matchMedia` listener: fires when the current devicePixelRatio
/// stops matching, re-sends the size, and re-arms for the new ratio.
fn arm_dpr_watch(send: std::rc::Rc<dyn Fn()>) {
    let Some(win) = web_sys::window() else { return };
    let dpr = win.device_pixel_ratio();
    let query = format!("(resolution: {dpr}dppx)");
    if let Ok(Some(mql)) = win.match_media(&query) {
        let send2 = send.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            send2();
            arm_dpr_watch(send2.clone());
        });
        mql.set_onchange(Some(cb.as_ref().unchecked_ref()));
        cb.forget();
    }
}

/// Worker thread: if `data` is a `{ kind: "resize", w, h }` message, resize the
/// `OffscreenCanvas` backing store (the renderer picks the new size up on its
/// next frame) and return the new size. Otherwise `None`.
pub fn try_apply_resize(canvas: &web_sys::OffscreenCanvas, data: &JsValue) -> Option<(u32, u32)> {
    let kind = js_sys::Reflect::get(data, &JsValue::from_str("kind"))
        .ok()?
        .as_string()?;
    if kind != "resize" {
        return None;
    }
    let w = (js_sys::Reflect::get(data, &JsValue::from_str("w"))
        .ok()?
        .as_f64()? as u32)
        .max(1);
    let h = (js_sys::Reflect::get(data, &JsValue::from_str("h"))
        .ok()?
        .as_f64()? as u32)
        .max(1);
    canvas.set_width(w);
    canvas.set_height(h);
    Some((w, h))
}

/// Worker thread: current backing aspect ratio (width / height).
pub fn aspect(canvas: &web_sys::OffscreenCanvas) -> f32 {
    (canvas.width().max(1) as f32) / (canvas.height().max(1) as f32)
}

/// Worker thread: install a scope `onmessage` that only handles resize messages
/// — for demos that need no other main→worker channel. Demos that already own
/// `onmessage` should call [`try_apply_resize`] inside their handler instead.
pub fn install_worker_resize(canvas: &web_sys::OffscreenCanvas) {
    use wasm_bindgen::JsCast;
    let canvas = canvas.clone();
    let cb = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
        let _ = try_apply_resize(&canvas, &e.data());
    });
    js_sys::global()
        .unchecked_into::<web_sys::DedicatedWorkerGlobalScope>()
        .set_onmessage(Some(cb.as_ref().unchecked_ref()));
    cb.forget();
}

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}
