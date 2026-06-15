//! Worker/main-thread boot logic + the `WorkerInputEvent` protocol.
//! See the [`crate`] module docs for the architecture overview.

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
    Resize {
        width: u32,
        height: u32,
    },
    PointerMove {
        x: i32,
        y: i32,
        buttons: u32,
    },
    PointerDown {
        x: i32,
        y: i32,
        buttons: u32,
    },
    PointerUp {
        x: i32,
        y: i32,
        buttons: u32,
    },
    Wheel {
        delta_x: f64,
        delta_y: f64,
    },
    KeyDown {
        code: String,
    },
    KeyUp {
        code: String,
    },
}

/// `true` when we're running inside a `DedicatedWorkerGlobalScope`.
pub fn is_worker_scope() -> bool {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .is_ok()
}

use std::sync::atomic::{AtomicBool, Ordering};

/// Tripped on first `boot()` to guard against trunk's auto-reload
/// re-invoking the start fn multiple times during a single page
/// session (each `cargo check` of an upstream crate can re-trigger
/// the inline boot script via trunk's WebSocket).
static BOOTED: AtomicBool = AtomicBool::new(false);

/// Single entry point. Routes to either main-thread or worker-side
/// boot based on the active global scope.
#[wasm_bindgen(start)]
pub fn boot() -> Result<(), JsValue> {
    if BOOTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
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

    // Attach an onerror so worker startup failures surface to the
    // main-thread console rather than silently disappearing.
    let onerror = Closure::<dyn FnMut(JsValue)>::new(|err: JsValue| {
        web_sys::console::error_2(&JsValue::from_str("render-worker error:"), &err);
    });
    worker.set_onerror(Some(onerror.as_ref().unchecked_ref::<js_sys::Function>()));
    onerror.forget();

    // And an onmessage so we can see what the worker (eventually)
    // reports back to us via the input-event protocol (or any
    // diagnostic messages it might emit).
    let onmessage = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        web_sys::console::log_2(&JsValue::from_str("render-worker msg:"), &e.data());
    });
    worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref::<js_sys::Function>()));
    onmessage.forget();

    let init_msg = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &init_msg,
        &JsValue::from_str("kind"),
        &JsValue::from_str("render-init"),
    );
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
    let on_pointer_move =
        Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |e: web_sys::PointerEvent| {
            let event = WorkerInputEvent::PointerMove {
                x: e.offset_x() as i32,
                y: e.offset_y() as i32,
                buttons: e.buttons() as u32,
            };
            if let Ok(js) = serde_wasm_bindgen::to_value(&event) {
                let _ = worker_for_move.post_message(&js);
            }
        });
    canvas.add_event_listener_with_callback(
        "pointermove",
        on_pointer_move.as_ref().unchecked_ref(),
    )?;
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

/// Worker-side bootstrap: installs an `onmessage` listener for
/// post-init input events. The `OffscreenCanvas` itself is delivered
/// to [`render_worker_start`] directly by the bootstrap JS (a
/// function call beats trying to postMessage-loop-back to the same
/// worker, since `self.postMessage` from a worker goes outward to
/// the main thread).
fn worker_thread_boot() -> Result<(), JsValue> {
    install_tracing();
    tracing::info!("render-worker example: worker boot");

    let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
    let scope_for_handler = scope.clone();

    let onmessage =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            // Input events from the main-thread shim — deserialize +
            // route. Real consumers feed these into a camera /
            // gameplay system; this example traces them.
            if let Ok(ev) = serde_wasm_bindgen::from_value::<WorkerInputEvent>(e.data()) {
                tracing::trace!("worker input: {:?}", ev);
            }
        });
    scope_for_handler.set_onmessage(Some(onmessage.as_ref().unchecked_ref::<js_sys::Function>()));
    onmessage.forget();
    Ok(())
}

/// Called directly by the worker-side bootstrap JS, *after*
/// `wbg.default(wasm_module)` returns (i.e. after `boot()` finished
/// installing the input-event listener). Receives the
/// `OffscreenCanvas` transferred from the main thread and drives the
/// renderer.
#[wasm_bindgen]
pub fn render_worker_start(canvas: web_sys::OffscreenCanvas) -> Result<(), JsValue> {
    start_worker_renderer(canvas)
}

/// Build the renderer against the OffscreenCanvas, populate it with
/// a procedural box, and start the rAF loop. Returns `Ok` once the
/// loop is armed; the closure keeps itself alive via `forget`.
fn start_worker_renderer(canvas: web_sys::OffscreenCanvas) -> Result<(), JsValue> {
    use awsm_materials::pbr::PbrMaterial;
    use awsm_materials::MaterialAlphaMode;
    use awsm_meshgen::primitives::box_mesh;
    use awsm_renderer::camera::CameraMatrices;
    use awsm_renderer::materials::Material;
    use awsm_renderer::raw_mesh::RawMeshData;
    use awsm_renderer::transforms::Transform;
    use awsm_renderer::AwsmRendererBuilder;
    use awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder;
    use glam::{Mat4, Vec3};

    let gpu = awsm_renderer::web_global::navigator_gpu()
        .ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas);

    wasm_bindgen_futures::spawn_local(async move {
        // Build the full renderer.
        let mut renderer = match AwsmRendererBuilder::new(gpu_builder).build().await {
            Ok(r) => r,
            Err(err) => {
                tracing::error!("worker: renderer build failed: {err}");
                return;
            }
        };
        tracing::info!("worker: renderer built");

        // Procedural box mesh + bright opaque PBR material.
        let mesh = box_mesh(Vec3::splat(1.0));
        let raw = RawMeshData {
            positions: mesh.positions,
            normals: mesh.normals,
            uvs: mesh.uvs,
            uvs1: None,
            colors: mesh.colors,
            indices: mesh.indices,
        };
        // High emissive factor so the box self-illuminates against
        // an empty scene with no punctual lights — the example
        // intentionally skips light + environment setup to keep the
        // smoke test focused on the OffscreenCanvas + render-loop
        // path.
        let mut mat = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
        mat.base_color_factor = [0.4, 0.7, 1.0, 1.0];
        mat.metallic_factor = 0.0;
        mat.roughness_factor = 0.6;
        mat.emissive_factor = [2.0, 4.0, 6.0];
        let material_key = renderer.materials.insert(
            Material::Pbr(Box::new(mat)),
            &renderer.textures,
            &renderer.dynamic_materials,
            &renderer.extras_pool,
        );
        let transform_key = renderer.transforms.insert(
            Transform {
                translation: Vec3::new(0.0, 0.0, -3.0),
                ..Default::default()
            },
            None,
        );
        if let Err(err) = renderer.add_raw_mesh(raw, transform_key, material_key) {
            tracing::error!("worker: add_raw_mesh failed: {err}");
            return;
        }

        // rAF loop via the worker-safe global helper.
        //
        // `Arc<Mutex<…>>` / `Arc<AtomicU32>` rather than
        // `Rc<RefCell<…>>` / `Rc<Cell<…>>` to keep the example aligned
        // with the renderer's "future-proof for multithreading"
        // convention. f32 has no atomic primitive; we bit-cast through
        // `AtomicU32` via `to_bits` / `from_bits` so the rotation
        // counter stays lock-free. Single-threaded today (the
        // `requestAnimationFrame` closure and the boot future share
        // the worker scope), so the atomic / lock cost is zero.
        //
        // `AwsmRenderer` became `!Send + !Sync` once the
        // `pipeline_scheduler`'s `FuturesUnordered` was added (its trait
        // objects don't carry an explicit `Send` bound — see
        // `crates/renderer/src/pipeline_scheduler/mod.rs`). Wasm32 is
        // single-threaded, so the lint is theoretical here.
        #[allow(clippy::arc_with_non_send_sync)]
        let renderer_cell = std::sync::Arc::new(std::sync::Mutex::new(renderer));
        let rotation_bits =
            std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0.0f32.to_bits()));
        // `Closure<dyn FnMut()>` from `wasm-bindgen` is `!Send + !Sync`
        // because it owns a JS function reference, so the
        // `Arc<Mutex<…>>` here can't actually move across threads
        // today. Kept Arc/Mutex anyway for consistency with the
        // renderer-wide convention ("future-proof for multithreading"
        // — see CLAUDE.md / the matching containers in
        // `workers/pool.rs`, `picker.rs`, `lib.rs`). The lint that
        // would flag this is suppressed.
        #[allow(clippy::arc_with_non_send_sync)]
        let raf_closure: std::sync::Arc<std::sync::Mutex<Option<Closure<dyn FnMut()>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let raf_closure_for_init = std::sync::Arc::clone(&raf_closure);
        let raf_closure_for_run = std::sync::Arc::clone(&raf_closure);
        let renderer_for_loop = std::sync::Arc::clone(&renderer_cell);
        let rotation_for_loop = std::sync::Arc::clone(&rotation_bits);
        let transform_key_for_loop = transform_key;

        *raf_closure_for_init.lock().unwrap() = Some(Closure::new(move || {
            // Spin the box gently so the smoke test confirms the
            // render loop is alive (frame-to-frame mutation visible).
            use std::sync::atomic::Ordering;
            let t = f32::from_bits(rotation_for_loop.load(Ordering::Relaxed)) + 0.01;
            rotation_for_loop.store(t.to_bits(), Ordering::Relaxed);
            {
                let mut r = renderer_for_loop.lock().unwrap();
                if let Ok(current) = r.transforms.get_local(transform_key_for_loop).cloned() {
                    let _ = r.transforms.set_local(
                        transform_key_for_loop,
                        Transform {
                            rotation: glam::Quat::from_rotation_y(t),
                            ..current
                        },
                    );
                }
                // Camera looking at the box from +Z. Recomputed every
                // frame so the renderer's `last_matrices`-based dirty
                // tracking marks the camera buffer dirty in the
                // initial frames.
                let view =
                    Mat4::look_at_rh(Vec3::new(0.0, 1.5, 3.0), Vec3::new(0.0, 0.0, -3.0), Vec3::Y);
                // Aspect = canvas width / height; the OffscreenCanvas
                // is fixed at 800x600 in index.html.
                let projection =
                    Mat4::perspective_rh(60.0_f32.to_radians(), 800.0 / 600.0, 0.1, 100.0);
                let _ = r.update_camera(CameraMatrices {
                    view,
                    projection,
                    position_world: Vec3::new(0.0, 1.5, 3.0),
                    focus_distance: 10.0,
                    aperture: 5.6,
                });
                r.update_transforms();
                if let Err(err) = r.render(None) {
                    tracing::warn!("worker: render error: {err}");
                }
            }
            // Re-arm.
            if let Some(closure) = raf_closure_for_run.lock().unwrap().as_ref() {
                let _ = awsm_renderer::web_global::request_animation_frame(
                    closure.as_ref().unchecked_ref(),
                );
            }
        }));
        if let Some(closure) = raf_closure_for_init.lock().unwrap().as_ref() {
            let _ = awsm_renderer::web_global::request_animation_frame(
                closure.as_ref().unchecked_ref(),
            );
        }
        // Closure lives in the Arc; intentionally leaked alongside
        // `renderer_cell` / `rotation_bits` for the lifetime of the
        // worker. The worker scope owns them via the `move` capture.
        std::mem::forget(raf_closure);
        std::mem::forget(renderer_cell);
        std::mem::forget(rotation_bits);
    });
    Ok(())
}

fn install_tracing() {
    use tracing_subscriber::prelude::*;
    // tracing-subscriber's default `fmt::layer()` time formatter
    // calls `SystemTime::now()` which panics on wasm32. `without_time`
    // strips the timestamp; the browser's console already prepends
    // its own timestamp anyway.
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(tracing_web::MakeWebConsoleWriter::new())
        .with_target(false);
    let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
}

/// Worker bootstrap JS. Mirrors `awsm_renderer::workers::blob::WORKER_BOOTSTRAP_JS`
/// but specialised for this example's init message shape — receives
/// the `OffscreenCanvas` transferred from the main thread alongside
/// the shared `WebAssembly.Module`, calls `render_worker_start(canvas)`
/// directly (postMessage-loopback-to-self doesn't work; messages from
/// inside a worker go outward to the main thread).
const WORKER_BOOTSTRAP_JS: &str = r#"
self.onmessage = async (e) => {
    if (e.data && e.data.kind === "render-init") {
        const { wasm_module, glue_url, canvas } = e.data;
        try {
            const wbg = await import(glue_url);
            await wbg.default(wasm_module);
            // boot() ran — it installed our worker input listener.
            // Hand the canvas to the renderer-start fn directly.
            wbg.render_worker_start(canvas);
        } catch (err) {
            console.error("render-worker init error:", err);
        }
    }
};
"#;

#[wasm_bindgen(inline_js = r#"
export function awsm_example_bundle_url() {
    // wasm-bindgen places this snippet at
    // `snippets/<crate>/inlineN.js` next to the main glue. The main
    // glue's filename has a build hash we can't predict at compile
    // time, so we recover it from the page's boot script (which
    // every trunk-built page emits as `import init from '/glue.js'`).
    if (typeof document !== "undefined") {
        const scripts = document.querySelectorAll("script[type=module]");
        for (const s of scripts) {
            const t = s.textContent || "";
            const m = t.match(/from\s+['"]([^'"]+\.js)['"]/);
            if (m) return new URL(m[1], location.href).href;
        }
    }
    // Fallback for non-DOM contexts; the worker bootstrap already
    // receives `glue_url` directly via the main-thread init message.
    return import.meta.url;
}
"#)]
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
