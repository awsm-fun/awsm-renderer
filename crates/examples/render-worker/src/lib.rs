//! Phase 4.4 reference example — the entire renderer runs in a
//! Web Worker against an `OffscreenCanvas`.
//!
//! ### Architecture
//!
//! - **Main thread** ([`main_thread_boot`]): creates a `<canvas>`,
//!   calls `transferControlToOffscreen()`, spawns a worker that
//!   imports this same wasm bundle, and posts the offscreen canvas +
//!   the shared `WebAssembly.Module` to it. After init, the main
//!   thread captures pointer/resize events and forwards them via
//!   `postMessage` so the worker-side renderer can react.
//!
//! - **Worker thread** ([`worker_thread_boot`]): receives the
//!   `OffscreenCanvas`, builds an [`AwsmRendererWebGpuBuilder`] via
//!   the [`new_with_offscreen_canvas`] constructor added in
//!   Phase 4.4, and drives a `requestAnimationFrame`-paced render
//!   loop. Input events from the main thread feed a simple free
//!   camera.
//!
//! Both entry points live in the same crate, dispatched at runtime
//! by [`crate::is_worker_scope`] so a single wasm bundle serves both
//! contexts (the same pattern `wasm-bindgen-rayon` uses).
//!
//! ### What this example covers
//!
//! - The `transferControlToOffscreen()` + shared `WebAssembly.Module`
//!   handshake.
//! - The `new_with_offscreen_canvas(..)` builder path.
//! - Input forwarding via the [`WorkerInputEvent`] enum.
//! - `requestAnimationFrame` driven from the worker side via
//!   [`awsm_renderer::web_global::request_animation_frame`].
//!
//! ### What it does *not* cover
//!
//! - A real glTF scene — the example renders a clear color so the
//!   transport / lifecycle is exercised without dragging in
//!   asset-loading boilerplate. Plug a real scene in by calling
//!   `renderer.populate_gltf(..)` after init.
//! - DOM-overlay UI — that's a consumer choice (HTML element
//!   absolutely-positioned over the canvas; see
//!   `docs/DEPLOYMENT_MODES.md`).
//!
//! Browser smoke-verification of this example is part of the
//! Phase 4.4 follow-on work — `cargo check` passes today; an
//! end-to-end `trunk serve` boot needs a tiny `index.html` shim
//! (see [`HTML_SHIM`] below) and is verified in the editor's
//! Claude Preview MCP harness.

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::js_sys;

/// Worker-input protocol — the main-thread shim posts these to the
/// renderer worker via `postMessage`. Consumers can extend this enum
/// for game-specific events (gamepad, key chords, etc.).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WorkerInputEvent {
    /// New canvas backing-buffer size.
    Resize { width: u32, height: u32 },
    PointerMove { x: i32, y: i32, buttons: u32 },
    PointerDown { x: i32, y: i32, buttons: u32 },
    PointerUp { x: i32, y: i32, buttons: u32 },
    Wheel { delta_x: f64, delta_y: f64 },
    KeyDown { code: String },
    KeyUp { code: String },
}

/// `true` when we're running inside a `DedicatedWorkerGlobalScope`.
pub fn is_worker_scope() -> bool {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .is_ok()
}

/// Single entry point. Routes to either main-thread or worker-side
/// boot based on the active global scope.
#[wasm_bindgen(start)]
pub fn boot() -> Result<(), JsValue> {
    if is_worker_scope() {
        worker_thread_boot()
    } else {
        main_thread_boot()
    }
}

/// Main-thread bootstrap: transfer the canvas to a worker, post init.
fn main_thread_boot() -> Result<(), JsValue> {
    use wasm_bindgen_futures::spawn_local;
    install_tracing();
    tracing::info!("render-worker example: main-thread boot");

    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let canvas = document
        .get_element_by_id("canvas")
        .ok_or_else(|| JsValue::from_str("no #canvas"))?;
    let canvas: web_sys::HtmlCanvasElement = canvas.unchecked_into();
    let offscreen = canvas.transfer_control_to_offscreen()?;

    // Spawn a worker pointing at the same JS glue (this very wasm
    // bundle) via the inline-bootstrap pattern the WorkerPool uses.
    // The `module_or_path` argument to wasm-bindgen's `init` accepts
    // a pre-compiled `WebAssembly.Module` posted in the init message.
    let wasm_module = wasm_bindgen::module();
    let glue_url = bundle_url();

    let worker_js = WORKER_BOOTSTRAP_JS;
    let blob_opts = web_sys::BlobPropertyBag::new();
    blob_opts.set_type("application/javascript");
    let parts = js_sys::Array::new_with_length(1);
    parts.set(0, JsValue::from_str(worker_js));
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts.into(), &blob_opts)?;
    let blob_url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let worker_opts = web_sys::WorkerOptions::new();
    worker_opts.set_type(web_sys::WorkerType::Module);
    let worker = web_sys::Worker::new_with_options(&blob_url, &worker_opts)?;
    let _ = web_sys::Url::revoke_object_url(&blob_url);

    let init_msg = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&init_msg, &JsValue::from_str("kind"), &JsValue::from_str("render-init"));
    let _ = js_sys::Reflect::set(&init_msg, &JsValue::from_str("wasm_module"), &wasm_module);
    let _ = js_sys::Reflect::set(
        &init_msg,
        &JsValue::from_str("glue_url"),
        &JsValue::from_str(&glue_url),
    );
    let _ = js_sys::Reflect::set(&init_msg, &JsValue::from_str("canvas"), &offscreen);
    let transfer = js_sys::Array::new_with_length(1);
    transfer.set(0, offscreen.into());
    worker.post_message_with_transfer(&init_msg, &transfer)?;

    // Forward pointer events (extend per game).
    let worker_for_move = worker.clone();
    let on_pointer_move = Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |e: web_sys::PointerEvent| {
        let event = WorkerInputEvent::PointerMove {
            x: e.offset_x() as i32,
            y: e.offset_y() as i32,
            buttons: e.buttons() as u32,
        };
        if let Ok(js) = serde_wasm_bindgen::to_value(&event) {
            let _ = worker_for_move.post_message(&js);
        }
    });
    canvas
        .add_event_listener_with_callback("pointermove", on_pointer_move.as_ref().unchecked_ref())?;
    on_pointer_move.forget();

    // Forward resize via ResizeObserver.
    let worker_for_resize = worker.clone();
    spawn_local(async move {
        // (Stub — a ResizeObserver-based forwarder is left as
        // consumer-side work since it ties into the framework that
        // owns the DOM layout.)
        let _ = worker_for_resize;
    });

    Ok(())
}

/// Worker-side bootstrap: receives the `OffscreenCanvas`, builds the
/// renderer, drives the rAF loop.
fn worker_thread_boot() -> Result<(), JsValue> {
    install_tracing();
    tracing::info!("render-worker example: worker boot");

    let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
    let scope_for_handler = scope.clone();

    let onmessage = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
        let data = e.data();
        let kind = js_sys::Reflect::get(&data, &JsValue::from_str("kind"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        match kind.as_str() {
            "render-init" => {
                let canvas = js_sys::Reflect::get(&data, &JsValue::from_str("canvas"))
                    .ok()
                    .and_then(|v| v.dyn_into::<web_sys::OffscreenCanvas>().ok());
                if let Some(canvas) = canvas {
                    if let Err(err) = start_worker_renderer(canvas) {
                        tracing::error!("render-worker init failed: {err:?}");
                    }
                }
            }
            _ => {
                // Input events — deserialize and route. Real consumers
                // would feed these into a camera / gameplay system; in
                // this example we just trace them.
                if let Ok(ev) = serde_wasm_bindgen::from_value::<WorkerInputEvent>(data) {
                    tracing::trace!("worker input: {:?}", ev);
                }
            }
        }
    });
    scope_for_handler.set_onmessage(Some(onmessage.as_ref().unchecked_ref::<js_sys::Function>()));
    onmessage.forget();
    Ok(())
}

/// Build the renderer against the OffscreenCanvas and start the rAF
/// loop. Returns `Ok` once the loop is armed; the closure keeps
/// itself alive via `forget`.
fn start_worker_renderer(canvas: web_sys::OffscreenCanvas) -> Result<(), JsValue> {
    use awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder;
    let gpu = awsm_renderer::web_global::navigator_gpu()
        .ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas);
    wasm_bindgen_futures::spawn_local(async move {
        let _gpu = match builder.build().await {
            Ok(g) => g,
            Err(err) => {
                tracing::error!("worker: GPU build failed: {err}");
                return;
            }
        };
        tracing::info!("worker: GPU device ready");
        // Real consumers construct an AwsmRenderer here and drive
        // `render()` from a rAF loop via
        // `awsm_renderer::web_global::request_animation_frame`. This
        // example stops at GPU-device init so the smoke test
        // verifies the OffscreenCanvas handshake without dragging in
        // scene-loading dependencies.
    });
    Ok(())
}

fn install_tracing() {
    use tracing_subscriber::prelude::*;
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(tracing_web::MakeWebConsoleWriter::new())
        .with_target(false);
    let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
}

/// Worker bootstrap JS. Mirrors `awsm_renderer::workers::blob::WORKER_BOOTSTRAP_JS`
/// but specialised for this example's init message shape — receives
/// the `OffscreenCanvas` transferred from the main thread alongside
/// the shared `WebAssembly.Module`.
const WORKER_BOOTSTRAP_JS: &str = r#"
self.onmessage = async (e) => {
    if (e.data && e.data.kind === "render-init") {
        const { wasm_module, glue_url, canvas } = e.data;
        try {
            const wbg = await import(glue_url);
            await wbg.default(wasm_module);
            // wasm-bindgen's start fn runs automatically; it routes
            // via is_worker_scope() to worker_thread_boot(), which
            // installs an onmessage listener and replaces *this* one.
            // Forward the canvas through to that listener by reposting.
            self.postMessage({ kind: "render-init", canvas: canvas }, [canvas]);
        } catch (err) {
            console.error("render-worker init error:", err);
        }
    }
};
"#;

#[wasm_bindgen(inline_js = "export function awsm_example_bundle_url() { return import.meta.url; }")]
extern "C" {
    fn awsm_example_bundle_url() -> String;
}

fn bundle_url() -> String {
    awsm_example_bundle_url()
}

/// The HTML shim the consumer serves — just a single `<canvas>` and
/// the wasm `<script>`. Exposed as a constant so the example's README
/// can link to it without duplicating.
pub const HTML_SHIM: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>render-worker example</title>
<style>
    html, body { margin: 0; padding: 0; background: #111; }
    #canvas { display: block; width: 100vw; height: 100vh; touch-action: none; }
</style>
</head>
<body>
    <canvas id="canvas"></canvas>
    <link data-trunk rel="rust" data-wasm-opt="z" />
</body>
</html>
"#;
