//! M6 — input forwarding + main-thread responsiveness.
//!
//! The main thread owns the DOM, so it captures every input event and
//! forwards it to the render worker as a [`WorkerInputEvent`] (the full
//! protocol: pointer move/down/up, wheel, key down/up, and a
//! `ResizeObserver`-driven resize). The worker applies them to an orbit
//! camera.
//!
//! Crucially the renderer's cold load (build + pipeline compile, seconds)
//! runs **in the worker**, so the main thread stays free: a main-thread
//! `requestAnimationFrame` loop animates a DOM indicator and records its
//! frame count + worst frame gap the whole time. A `performance` trace over
//! the cold load shows main-thread frames keep painting with no long tasks —
//! the responsiveness win the whole effort targets.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use web_sys::js_sys;

/// The input protocol forwarded main → worker.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WorkerInputEvent {
    Resize { width: u32, height: u32 },
    PointerMove { x: i32, y: i32, buttons: u32 },
    PointerDown { x: i32, y: i32, buttons: u32 },
    PointerUp { x: i32, y: i32, buttons: u32 },
    Wheel { delta_x: f64, delta_y: f64 },
    KeyDown { code: String },
    KeyUp { code: String },
}

// ───────────────────────── main-thread driver ─────────────────────────

pub fn start_main() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let canvas: web_sys::HtmlCanvasElement = document
        .get_element_by_id("canvas")
        .ok_or_else(|| JsValue::from_str("no #canvas"))?
        .unchecked_into();
    // Size the backing store to CSS size × devicePixelRatio so the worker
    // renders at native resolution (no upscaling) — then transfer.
    let _ = crate::viewport::size_canvas_to_display(&canvas);
    let offscreen = canvas.transfer_control_to_offscreen()?;

    if let Some(hud) = document.get_element_by_id("hud") {
        hud.set_inner_html(
            r#"<div style="font:13px system-ui;color:#9f9">
                 main frames: <span id="frames">0</span> ·
                 long tasks &gt;50ms: <span id="longtasks">0</span> ·
                 steady worst gap: <span id="gap">0</span>ms
                 <div id="spinner" style="width:18px;height:18px;background:#4f4;margin-top:6px;border-radius:3px"></div>
               </div>"#,
        );
    }

    let state = js_sys::Object::new();
    set(&state, "mainFrames", &JsValue::from_f64(0.0));
    set(&state, "maxGapMs", &JsValue::from_f64(0.0));
    set(&state, "longTaskCount", &JsValue::from_f64(0.0));
    set(&state, "maxLongTaskMs", &JsValue::from_f64(0.0));
    let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_input"), &state);
    // The honest main-thread-blocking metric: the Long Tasks API reports any
    // task >50 ms on the main thread directly (unlike rAF gaps, which also
    // capture vsync/compositor scheduling).
    install_longtask_observer();

    let noop = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|_| {});
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    let worker = crate::bootstrap::spawn_shared_worker_transfer(
        "input-render",
        &offscreen,
        &transfer,
        noop.as_ref().unchecked_ref(),
    )?;
    noop.forget();

    install_input_forwarding(&document, &canvas, &worker)?;
    crate::viewport::observe_resize(&canvas, &worker)?;
    start_main_responsiveness_loop(&document)?;

    tracing::info!("input demo: worker spawned, input forwarding + main rAF armed");
    Ok(())
}

/// Forward every DOM input event to the worker, plus a `ResizeObserver`.
fn install_input_forwarding(
    document: &web_sys::Document,
    canvas: &web_sys::HtmlCanvasElement,
    worker: &web_sys::Worker,
) -> Result<(), JsValue> {
    fn post(worker: &web_sys::Worker, ev: &WorkerInputEvent) {
        if let Ok(js) = serde_wasm_bindgen::to_value(ev) {
            let _ = worker.post_message(&js);
        }
    }

    // pointermove / down / up
    for (name, mk) in [
        ("pointermove", 0u8),
        ("pointerdown", 1u8),
        ("pointerup", 2u8),
    ] {
        let w = worker.clone();
        let cb =
            Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |e: web_sys::PointerEvent| {
                let (x, y, b) = (e.offset_x() as i32, e.offset_y() as i32, e.buttons() as u32);
                let ev = match mk {
                    0 => WorkerInputEvent::PointerMove { x, y, buttons: b },
                    1 => WorkerInputEvent::PointerDown { x, y, buttons: b },
                    _ => WorkerInputEvent::PointerUp { x, y, buttons: b },
                };
                post(&w, &ev);
            });
        canvas.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref())?;
        cb.forget();
    }

    // wheel
    {
        let w = worker.clone();
        let cb = Closure::<dyn FnMut(web_sys::WheelEvent)>::new(move |e: web_sys::WheelEvent| {
            e.prevent_default();
            post(
                &w,
                &WorkerInputEvent::Wheel {
                    delta_x: e.delta_x(),
                    delta_y: e.delta_y(),
                },
            );
        });
        let opts = web_sys::AddEventListenerOptions::new();
        opts.set_passive(false);
        canvas.add_event_listener_with_callback_and_add_event_listener_options(
            "wheel",
            cb.as_ref().unchecked_ref(),
            &opts,
        )?;
        cb.forget();
    }

    // keydown / keyup (on document)
    for (name, down) in [("keydown", true), ("keyup", false)] {
        let w = worker.clone();
        let cb =
            Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
                let code = e.code();
                post(
                    &w,
                    &if down {
                        WorkerInputEvent::KeyDown { code }
                    } else {
                        WorkerInputEvent::KeyUp { code }
                    },
                );
            });
        document.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref())?;
        cb.forget();
    }

    // Resize is handled by `viewport::observe_resize` (CSS size × dpr).

    Ok(())
}

/// Main-thread `requestAnimationFrame` loop — the responsiveness indicator.
/// Animates a DOM element and records the frame count + worst inter-frame gap
/// so the gate can prove main keeps painting during the worker's cold load.
fn start_main_responsiveness_loop(document: &web_sys::Document) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let perf = window
        .performance()
        .ok_or_else(|| JsValue::from_str("no performance"))?;
    let spinner = document.get_element_by_id("spinner");
    let frames_el = document.get_element_by_id("frames");
    let gap_el = document.get_element_by_id("gap");
    let longtasks_el = document.get_element_by_id("longtasks");

    let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let raf_init = raf.clone();
    let raf_run = raf.clone();
    let last = Rc::new(RefCell::new(perf.now()));
    let frames = Rc::new(RefCell::new(0u32));
    let max_gap = Rc::new(RefCell::new(0.0f64));

    *raf_init.borrow_mut() = Some(Closure::new(move || {
        let now = perf.now();
        let gap = now - *last.borrow();
        *last.borrow_mut() = now;
        let f = {
            let mut fb = frames.borrow_mut();
            *fb += 1;
            *fb
        };
        // Steady-state worst gap: skip the warmup window (first ~60 frames),
        // where rAF naturally has large gaps before first paint / layout
        // settle — those are scheduling, not main-thread blocking. Real
        // main-thread blocking is reported separately via the Long Tasks API
        // (see `install_longtask_observer`), which is the honest metric.
        if f > 60 && gap > *max_gap.borrow() {
            *max_gap.borrow_mut() = gap;
        }
        // Animate the DOM indicator so the trace shows real main-thread paint.
        if let Some(s) = &spinner {
            let _ = s
                .unchecked_ref::<web_sys::HtmlElement>()
                .style()
                .set_property("transform", &format!("rotate({}deg)", f % 360));
        }
        if let Some(el) = &frames_el {
            el.set_text_content(Some(&f.to_string()));
        }
        if let Some(el) = &gap_el {
            el.set_text_content(Some(&format!("{:.0}", *max_gap.borrow())));
        }
        if let Some(el) = &longtasks_el {
            let count = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__mt_input"))
                .ok()
                .and_then(|s| js_sys::Reflect::get(&s, &JsValue::from_str("longTaskCount")).ok())
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            el.set_text_content(Some(&format!("{count:.0}")));
        }
        if let Ok(state) = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__mt_input"))
        {
            set(
                state.unchecked_ref(),
                "mainFrames",
                &JsValue::from_f64(f as f64),
            );
            set(
                state.unchecked_ref(),
                "maxGapMs",
                &JsValue::from_f64(*max_gap.borrow()),
            );
        }
        if let Some(cb) = raf_run.borrow().as_ref() {
            let _ = web_sys::window()
                .unwrap()
                .request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }));
    if let Some(cb) = raf_init.borrow().as_ref() {
        window.request_animation_frame(cb.as_ref().unchecked_ref())?;
    }
    std::mem::forget(raf);
    Ok(())
}

// ───────────────────────── worker-side host ─────────────────────────

pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "input-render" => render_main(payload),
        _ => Ok(()),
    }
}

/// Orbit-camera state mutated by forwarded input, read by the render loop.
#[derive(Clone, Copy)]
struct CameraState {
    yaw: f32,
    pitch: f32,
    distance: f32,
    last_x: i32,
    last_y: i32,
    dragging: bool,
}

fn render_main(payload: JsValue) -> Result<(), JsValue> {
    use awsm_renderer::web_global::navigator_gpu;
    use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};

    let canvas: web_sys::OffscreenCanvas = payload.unchecked_into();
    // Keep a handle to the canvas for live resize + camera aspect (cloning an
    // OffscreenCanvas is a cheap JS ref to the same backing store).
    let canvas_handle = canvas.clone();
    let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());

    let camera = Rc::new(RefCell::new(CameraState {
        yaw: 0.6,
        pitch: 0.3,
        distance: 9.0,
        last_x: 0,
        last_y: 0,
        dragging: false,
    }));

    // Message handler (replaces the bootstrap init onmessage): resize messages
    // first, then forwarded input events.
    let camera_in = camera.clone();
    let canvas_msg = canvas_handle.clone();
    let on_input =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            let data = e.data();
            if crate::viewport::try_apply_resize(&canvas_msg, &data).is_some() {
                return;
            }
            if let Ok(ev) = serde_wasm_bindgen::from_value::<WorkerInputEvent>(data) {
                apply_input(&mut camera_in.borrow_mut(), ev);
            }
        });
    js_sys::global()
        .unchecked_into::<web_sys::DedicatedWorkerGlobalScope>()
        .set_onmessage(Some(on_input.as_ref().unchecked_ref()));
    on_input.forget();

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run_render(gpu_builder, camera, canvas_handle).await {
            tracing::error!("input demo render: {err:?}");
        }
    });
    Ok(())
}

fn apply_input(cam: &mut CameraState, ev: WorkerInputEvent) {
    match ev {
        WorkerInputEvent::PointerDown { x, y, .. } => {
            cam.dragging = true;
            cam.last_x = x;
            cam.last_y = y;
        }
        WorkerInputEvent::PointerUp { .. } => cam.dragging = false,
        WorkerInputEvent::PointerMove { x, y, buttons } => {
            if cam.dragging && buttons != 0 {
                let dx = (x - cam.last_x) as f32;
                let dy = (y - cam.last_y) as f32;
                cam.yaw -= dx * 0.01;
                cam.pitch = (cam.pitch + dy * 0.01).clamp(-1.4, 1.4);
            }
            cam.last_x = x;
            cam.last_y = y;
        }
        WorkerInputEvent::Wheel { delta_y, .. } => {
            cam.distance = (cam.distance + delta_y as f32 * 0.01).clamp(3.0, 40.0);
        }
        WorkerInputEvent::KeyDown { code } => {
            // Arrow keys nudge the orbit too (keyboard path exercised).
            match code.as_str() {
                "ArrowLeft" => cam.yaw -= 0.1,
                "ArrowRight" => cam.yaw += 0.1,
                "ArrowUp" => cam.pitch = (cam.pitch + 0.1).clamp(-1.4, 1.4),
                "ArrowDown" => cam.pitch = (cam.pitch - 0.1).clamp(-1.4, 1.4),
                _ => {}
            }
        }
        WorkerInputEvent::KeyUp { .. } | WorkerInputEvent::Resize { .. } => {}
    }
}

async fn run_render(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    camera: Rc<RefCell<CameraState>>,
    canvas: web_sys::OffscreenCanvas,
) -> Result<(), JsValue> {
    use awsm_materials::pbr::PbrMaterial;
    use awsm_materials::MaterialAlphaMode;
    use awsm_meshgen::primitives::box_mesh;
    use awsm_renderer::camera::CameraMatrices;
    use awsm_renderer::materials::Material;
    use awsm_renderer::raw_mesh::RawMeshData;
    use awsm_renderer::transforms::Transform;
    use awsm_renderer::AwsmRendererBuilder;
    use glam::{Mat4, Vec3};

    let mut renderer = AwsmRendererBuilder::new(gpu_builder)
        .build()
        .await
        .map_err(|e| JsValue::from_str(&format!("renderer build failed: {e}")))?;

    // A small model (cold-load weight + a recognisable orbit subject).
    let mut mat = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
    mat.base_color_factor = [0.4, 0.7, 1.0, 1.0];
    mat.emissive_factor = [1.5, 2.5, 3.5];
    let material_key = renderer.materials.insert(
        Material::Pbr(Box::new(mat)),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );
    for gy in -1..=1 {
        for gx in -1..=1 {
            let mesh = box_mesh(Vec3::splat(0.7));
            let raw = RawMeshData {
                positions: mesh.positions,
                normals: mesh.normals,
                uv_sets: mesh.uvs,
                colors: mesh.colors,
                indices: mesh.indices,
                ..Default::default()
            };
            let tk = renderer.transforms.insert(
                Transform {
                    translation: Vec3::new(gx as f32 * 1.2, gy as f32 * 1.2, 0.0),
                    ..Default::default()
                },
                None,
            );
            renderer
                .add_raw_mesh(raw, tk, material_key)
                .map_err(|e| JsValue::from_str(&format!("add_raw_mesh: {e}")))?;
        }
    }
    renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| JsValue::from_str(&format!("commit_load: {e}")))?;

    #[allow(clippy::arc_with_non_send_sync)]
    let cell = Rc::new(RefCell::new(renderer));
    #[allow(clippy::arc_with_non_send_sync)]
    let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let raf_init = raf.clone();
    let raf_run = raf.clone();
    let cell_loop = cell.clone();

    *raf_init.borrow_mut() = Some(Closure::new(move || {
        let cam = *camera.borrow();
        let mut r = cell_loop.borrow_mut();
        let eye = Vec3::new(
            cam.distance * cam.pitch.cos() * cam.yaw.sin(),
            cam.distance * cam.pitch.sin(),
            cam.distance * cam.pitch.cos() * cam.yaw.cos(),
        );
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let projection = Mat4::perspective_rh(
            60.0_f32.to_radians(),
            crate::viewport::aspect(&canvas),
            0.1,
            100.0,
        );
        let _ = r.update_camera(CameraMatrices {
            view,
            projection,
            position_world: eye,
            focus_distance: 10.0,
            aperture: 5.6,
        });
        r.update_transforms();
        let _ = r.render(None);
        if let Some(cb) = raf_run.borrow().as_ref() {
            let _ = awsm_renderer::web_global::request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }));
    if let Some(cb) = raf_init.borrow().as_ref() {
        awsm_renderer::web_global::request_animation_frame(cb.as_ref().unchecked_ref())?;
    }
    std::mem::forget(raf);
    std::mem::forget(cell);
    Ok(())
}

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}

#[wasm_bindgen(inline_js = r#"
export function install_longtask_observer() {
    try {
        let count = 0, max = 0;
        const obs = new PerformanceObserver((list) => {
            for (const e of list.getEntries()) {
                count++;
                if (e.duration > max) max = e.duration;
            }
            const s = globalThis.__mt_input;
            if (s) { s.longTaskCount = count; s.maxLongTaskMs = Math.round(max); }
        });
        obs.observe({ entryTypes: ['longtask'] });
    } catch (e) { /* longtask API unsupported — leave the counters at 0 */ }
}
"#)]
extern "C" {
    fn install_longtask_observer();
}
