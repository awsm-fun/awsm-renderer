//! M5 — full Layer 1 remote-renderer protocol in action.
//!
//! A main-thread DOM driver fully controls a worker-hosted renderer over the
//! typed [`RenderCommand`]/[`RenderEvent`] channel ([`crate::protocol`]):
//! - The driver builds a small model on the main thread, ships each mesh's
//!   geometry as a **Transferable** `ArrayBuffer` (zero-copy), and sends a
//!   `Load` command.
//! - The worker reconstructs the meshes, runs the load transaction off-main,
//!   and streams `Loading(LoadingStats)` events; the driver paints a DOM
//!   progress bar from them (the responsiveness win — the DOM paints each
//!   phase for free while the worker compiles).
//! - After `Ready`, the driver issues a `Pick` at the model centre; the
//!   worker round-trips a `PickResult`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use web_sys::js_sys;

use crate::protocol::{phase_fraction, ModelDesc, RenderCommand, RenderEvent};

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
    let _ = crate::viewport::size_canvas_to_display(&canvas);
    let offscreen = canvas.transfer_control_to_offscreen()?;

    // DOM progress bar overlay (the thing that paints from Loading events).
    if let Some(hud) = document.get_element_by_id("hud") {
        hud.set_inner_html(
            r#"<div style="font:14px system-ui;color:#ddd">
                 <div id="status">connecting…</div>
                 <div style="width:300px;height:14px;background:#333;border-radius:7px;margin-top:6px">
                   <div id="bar" style="width:0%;height:100%;background:#4af;border-radius:7px;transition:width .1s"></div>
                 </div>
               </div>"#,
        );
    }

    // Expose a state object for the gate.
    let state = js_sys::Object::new();
    set(&state, "phase", &JsValue::from_str("connecting"));
    set(&state, "ready", &JsValue::from_bool(false));
    set(&state, "events", &js_sys::Array::new());
    let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_remote"), &state);

    let worker: Rc<RefCell<Option<web_sys::Worker>>> = Rc::new(RefCell::new(None));
    let worker_for_msg = worker.clone();
    let on_msg =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            handle_event(&e, &worker_for_msg);
        });
    let w = crate::bootstrap::spawn_shared_worker_transfer(
        "remote-render",
        &offscreen,
        &{
            let a = js_sys::Array::new();
            a.push(&offscreen);
            a
        },
        on_msg.as_ref().unchecked_ref(),
    )?;
    crate::viewport::observe_resize(&canvas, &w)?;
    *worker.borrow_mut() = Some(w);
    on_msg.forget();
    tracing::info!("remote demo: spawned worker, awaiting Initialized");
    Ok(())
}

/// Route a worker → main `RenderEvent`.
fn handle_event(e: &web_sys::MessageEvent, worker: &Rc<RefCell<Option<web_sys::Worker>>>) {
    let data = e.data();
    if js_sys::Reflect::get(&data, &JsValue::from_str("kind"))
        .ok()
        .and_then(|v| v.as_string())
        .as_deref()
        != Some("evt")
    {
        return;
    }
    let evt_val = match js_sys::Reflect::get(&data, &JsValue::from_str("evt")) {
        Ok(v) => v,
        Err(_) => return,
    };
    let event: RenderEvent = match serde_wasm_bindgen::from_value(evt_val) {
        Ok(ev) => ev,
        Err(err) => {
            tracing::warn!("remote demo: bad event: {err:?}");
            return;
        }
    };
    let document = web_sys::window().and_then(|w| w.document());
    let set_state = |k: &str, v: &JsValue| {
        if let Ok(state) =
            js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__mt_remote"))
        {
            let _ = js_sys::Reflect::set(&state, &JsValue::from_str(k), v);
        }
    };

    match event {
        RenderEvent::Initialized => {
            tracing::info!("remote demo: worker Initialized — sending Load");
            set_state("phase", &JsValue::from_str("loading"));
            if let Some(w) = worker.borrow().as_ref() {
                if let Err(err) = send_load(w) {
                    tracing::error!("remote demo: send_load failed: {err:?}");
                }
            }
        }
        RenderEvent::Loading {
            phase_label,
            phase,
            geometry_uploaded,
            geometry_total,
            pipelines_ready,
            pipelines_pending,
            ..
        } => {
            // Recompute the bar fraction from the same mapping the worker used.
            let frac = match phase {
                1 => {
                    0.40 * if geometry_total == 0 {
                        1.0
                    } else {
                        geometry_uploaded as f32 / geometry_total as f32
                    }
                }
                2 => 0.45,
                3 => {
                    let total = pipelines_ready + pipelines_pending;
                    0.50 + 0.50
                        * if total == 0 {
                            1.0
                        } else {
                            pipelines_ready as f32 / total as f32
                        }
                }
                _ => 0.0,
            };
            if let Some(doc) = &document {
                if let Some(bar) = doc.get_element_by_id("bar") {
                    let _ = bar
                        .unchecked_ref::<web_sys::HtmlElement>()
                        .style()
                        .set_property("width", &format!("{:.0}%", frac * 100.0));
                }
                if let Some(status) = doc.get_element_by_id("status") {
                    status.set_text_content(Some(&phase_label));
                }
            }
            set_state("phase", &JsValue::from_str(&phase_label));
            // Append to the streamed-phase history (proof the DOM painted each
            // Loading event off-main).
            if let Ok(state) =
                js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__mt_remote"))
            {
                if let Ok(events) = js_sys::Reflect::get(&state, &JsValue::from_str("events")) {
                    if let Ok(arr) = events.dyn_into::<js_sys::Array>() {
                        arr.push(&JsValue::from_str(&phase_label));
                    }
                }
            }
        }
        RenderEvent::Ready => {
            tracing::info!("remote demo: Ready — model loaded, sending Pick at centre");
            if let Some(doc) = &document {
                if let Some(bar) = doc.get_element_by_id("bar") {
                    let _ = bar
                        .unchecked_ref::<web_sys::HtmlElement>()
                        .style()
                        .set_property("width", "100%");
                }
                if let Some(status) = doc.get_element_by_id("status") {
                    status.set_text_content(Some("loaded"));
                }
            }
            set_state("phase", &JsValue::from_str("loaded"));
            set_state("ready", &JsValue::from_bool(true));
            // Pick the centre of the canvas (where the model is).
            if let Some(w) = worker.borrow().as_ref() {
                let _ = send_command(
                    w,
                    &RenderCommand::Pick { x: 400, y: 300 },
                    &js_sys::Array::new(),
                );
            }
        }
        RenderEvent::PickResult { hit, mesh_id } => {
            tracing::info!("remote demo: PickResult hit={hit} mesh_id={mesh_id}");
            set_state("pickHit", &JsValue::from_bool(hit));
            set_state("pickMeshId", &JsValue::from_f64(mesh_id));
        }
        RenderEvent::Error { message } => {
            tracing::error!("remote demo: worker error: {message}");
            set_state("error", &JsValue::from_str(&message));
        }
    }
}

/// Build a small model on the main thread and ship it as a `Load` command with
/// Transferable geometry buffers.
fn send_load(worker: &web_sys::Worker) -> Result<(), JsValue> {
    use awsm_meshgen::primitives::box_mesh;
    use glam::Vec3;

    let buffers = js_sys::Array::new();
    let mut models = Vec::new();
    let grid = 4i32;
    for gz in 0..2 {
        for gy in -grid / 2..=grid / 2 {
            for gx in -grid / 2..=grid / 2 {
                let mesh = box_mesh(Vec3::splat(0.7));
                // positions → f32 xyz bytes
                let mut pos_bytes: Vec<u8> = Vec::with_capacity(mesh.positions.len() * 12);
                for p in &mesh.positions {
                    for c in p {
                        pos_bytes.extend_from_slice(&c.to_le_bytes());
                    }
                }
                // indices → u32 bytes
                let mut idx_bytes: Vec<u8> = Vec::with_capacity(mesh.indices.len() * 4);
                for i in &mesh.indices {
                    idx_bytes.extend_from_slice(&i.to_le_bytes());
                }
                let pos_arr = js_sys::Uint8Array::from(pos_bytes.as_slice());
                let idx_arr = js_sys::Uint8Array::from(idx_bytes.as_slice());
                let pos_idx = buffers.length();
                buffers.push(&pos_arr);
                let idx_idx = buffers.length();
                buffers.push(&idx_arr);
                models.push(ModelDesc {
                    positions_buf: pos_idx,
                    indices_buf: idx_idx,
                    translation: [gx as f32 * 1.1, gy as f32 * 1.1, gz as f32],
                    color: [0.3 + 0.2 * gx as f32, 0.6, 0.9 - 0.1 * gy as f32, 1.0],
                });
            }
        }
    }
    send_command(worker, &RenderCommand::Load { models }, &buffers)
}

/// Post a command `{kind:"cmd", cmd, buffers}`, transferring every buffer's
/// `ArrayBuffer` (zero-copy).
fn send_command(
    worker: &web_sys::Worker,
    cmd: &RenderCommand,
    buffers: &js_sys::Array,
) -> Result<(), JsValue> {
    let msg = js_sys::Object::new();
    set(&msg, "kind", &JsValue::from_str("cmd"));
    set(
        &msg,
        "cmd",
        &serde_wasm_bindgen::to_value(cmd).map_err(|e| JsValue::from_str(&e.to_string()))?,
    );
    set(&msg, "buffers", buffers);
    let transfer = js_sys::Array::new();
    for i in 0..buffers.length() {
        let buf = buffers.get(i).unchecked_into::<js_sys::Uint8Array>();
        transfer.push(&buf.buffer());
    }
    if transfer.length() == 0 {
        worker.post_message(&msg)
    } else {
        worker.post_message_with_transfer(&msg, &transfer)
    }
}

// ───────────────────────── worker-side host ─────────────────────────

pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "remote-render" => render_main(payload),
        _ => Ok(()),
    }
}

fn render_main(payload: JsValue) -> Result<(), JsValue> {
    use awsm_renderer::web_global::navigator_gpu;
    use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};

    let canvas: web_sys::OffscreenCanvas = payload.unchecked_into();
    let canvas_handle = canvas.clone();
    let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run_worker(gpu_builder, canvas_handle).await {
            tracing::error!("remote demo worker: {err:?}");
        }
    });
    Ok(())
}

fn worker_scope() -> web_sys::DedicatedWorkerGlobalScope {
    js_sys::global().unchecked_into()
}

fn post_event(evt: &RenderEvent) {
    if let Ok(v) = serde_wasm_bindgen::to_value(evt) {
        let msg = js_sys::Object::new();
        set(&msg, "kind", &JsValue::from_str("evt"));
        set(&msg, "evt", &v);
        let _ = worker_scope().post_message(&msg);
    }
}

async fn run_worker(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    canvas: web_sys::OffscreenCanvas,
) -> Result<(), JsValue> {
    use awsm_renderer::camera::CameraMatrices;
    use awsm_renderer::features::RendererFeatures;
    use awsm_renderer::AwsmRendererBuilder;
    use glam::{Mat4, Vec3};

    let renderer = AwsmRendererBuilder::new(gpu_builder)
        .with_features(RendererFeatures {
            picking: true,
            ..Default::default()
        })
        .build()
        .await
        .map_err(|e| JsValue::from_str(&format!("renderer build failed: {e}")))?;

    #[allow(clippy::arc_with_non_send_sync)]
    let cell = Rc::new(RefCell::new(renderer));
    let loading = Rc::new(Cell::new(false));

    // Render loop — gated by `loading` so a command's async borrow (commit_load
    // / pick) never collides with the per-frame borrow.
    {
        #[allow(clippy::arc_with_non_send_sync)]
        let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
        let raf_init = raf.clone();
        let raf_run = raf.clone();
        let cell_loop = cell.clone();
        let loading_loop = loading.clone();
        let canvas_loop = canvas.clone();
        *raf_init.borrow_mut() = Some(Closure::new(move || {
            if !loading_loop.get() {
                if let Ok(mut r) = cell_loop.try_borrow_mut() {
                    let eye = Vec3::new(0.0, 0.0, 6.0);
                    let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
                    let projection = Mat4::perspective_rh(
                        60.0_f32.to_radians(),
                        crate::viewport::aspect(&canvas_loop),
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
                }
            }
            if let Some(cb) = raf_run.borrow().as_ref() {
                let _ =
                    awsm_renderer::web_global::request_animation_frame(cb.as_ref().unchecked_ref());
            }
        }));
        if let Some(cb) = raf_init.borrow().as_ref() {
            awsm_renderer::web_global::request_animation_frame(cb.as_ref().unchecked_ref())?;
        }
        std::mem::forget(raf);
    }

    // Command channel — replaces the bootstrap's init onmessage (init is done).
    // Resize messages are handled inline; everything else is a RenderCommand.
    let cell_cmd = cell.clone();
    let loading_cmd = loading.clone();
    let canvas_cmd = canvas.clone();
    let on_cmd =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            if crate::viewport::try_apply_resize(&canvas_cmd, &e.data()).is_some() {
                return;
            }
            handle_command(e, &cell_cmd, &loading_cmd);
        });
    worker_scope().set_onmessage(Some(on_cmd.as_ref().unchecked_ref()));
    on_cmd.forget();

    post_event(&RenderEvent::Initialized);
    std::mem::forget(cell);
    Ok(())
}

// The command futures deliberately hold the renderer borrow across `.await`
// (commit_load / pick are `&mut self` async). This is sound here: the render
// loop uses `try_borrow_mut` and skips a frame while a command owns the
// renderer (plus the `loading` flag), so there's no aliasing panic.
#[allow(clippy::await_holding_refcell_ref)]
fn handle_command(
    e: web_sys::MessageEvent,
    cell: &Rc<RefCell<awsm_renderer::AwsmRenderer>>,
    loading: &Rc<Cell<bool>>,
) {
    let data = e.data();
    if js_sys::Reflect::get(&data, &JsValue::from_str("kind"))
        .ok()
        .and_then(|v| v.as_string())
        .as_deref()
        != Some("cmd")
    {
        return;
    }
    let cmd_val = match js_sys::Reflect::get(&data, &JsValue::from_str("cmd")) {
        Ok(v) => v,
        Err(_) => return,
    };
    let cmd: RenderCommand = match serde_wasm_bindgen::from_value(cmd_val) {
        Ok(c) => c,
        Err(err) => {
            post_event(&RenderEvent::Error {
                message: format!("bad command: {err}"),
            });
            return;
        }
    };
    let buffers = js_sys::Reflect::get(&data, &JsValue::from_str("buffers"))
        .ok()
        .and_then(|v| v.dyn_into::<js_sys::Array>().ok())
        .unwrap_or_default();

    match cmd {
        RenderCommand::Load { models } => {
            let cell = cell.clone();
            let loading = loading.clone();
            loading.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                let res = load_models(&cell, &models, &buffers).await;
                if let Err(err) = res {
                    post_event(&RenderEvent::Error {
                        message: format!("{err:?}"),
                    });
                }
                loading.set(false);
            });
        }
        RenderCommand::Pick { x, y } => {
            let cell = cell.clone();
            let loading = loading.clone();
            loading.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                let result = {
                    let mut r = cell.borrow_mut();
                    r.pick(x, y).await
                };
                let (hit, mesh_id) = match result {
                    Ok(pr) => (pr.mesh_key().is_some(), 1.0),
                    Err(_) => (false, 0.0),
                };
                post_event(&RenderEvent::PickResult { hit, mesh_id });
                loading.set(false);
            });
        }
    }
}

/// Reconstruct each mesh from its Transferable buffers, add it, and run the
/// load transaction streaming `Loading` events.
#[allow(clippy::await_holding_refcell_ref)]
async fn load_models(
    cell: &Rc<RefCell<awsm_renderer::AwsmRenderer>>,
    models: &[ModelDesc],
    buffers: &js_sys::Array,
) -> Result<(), JsValue> {
    use awsm_materials::pbr::PbrMaterial;
    use awsm_materials::MaterialAlphaMode;
    use awsm_renderer::materials::Material;
    use awsm_renderer::raw_mesh::RawMeshData;
    use awsm_renderer::transforms::Transform;
    use glam::Vec3;

    let mut guard = cell.borrow_mut();
    // Reborrow to a plain `&mut AwsmRenderer` so disjoint field borrows
    // (`materials.insert(&textures, …)`) work — a `RefMut` deref would
    // borrow the whole renderer for the receiver and conflict with the args.
    let r = &mut *guard;
    for m in models {
        let positions = read_vec3(buffers, m.positions_buf);
        let indices = read_u32(buffers, m.indices_buf);
        let raw = RawMeshData {
            positions,
            indices,
            ..Default::default()
        };
        let mut mat = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
        mat.base_color_factor = m.color;
        mat.emissive_factor = [m.color[0] * 1.5, m.color[1] * 1.5, m.color[2] * 1.5];
        let material_key = r.materials.insert(
            Material::Pbr(Box::new(mat)),
            &r.textures,
            &r.dynamic_materials,
            &r.extras_pool,
        );
        let tk = r.transforms.insert(
            Transform {
                translation: Vec3::from_array(m.translation),
                ..Default::default()
            },
            None,
        );
        r.add_raw_mesh(raw, tk, material_key)
            .map_err(|e| JsValue::from_str(&format!("add_raw_mesh: {e}")))?;
    }

    // Run the commit, streaming each progress tick as a Loading event.
    r.commit_load(|stats| {
        let (phase, _) = phase_fraction(&stats);
        post_event(&RenderEvent::Loading {
            phase,
            phase_label: stats
                .phase_label()
                .unwrap_or_else(|| "Finishing".to_string()),
            geometry_uploaded: stats.geometry_uploaded,
            geometry_total: stats.geometry_total,
            textures_uploaded: stats.textures_uploaded,
            textures_total: stats.textures_total,
            pipelines_pending: stats.pipelines_pending,
            pipelines_ready: stats.pipelines_ready,
        });
    })
    .await
    .map_err(|e| JsValue::from_str(&format!("commit_load: {e}")))?;

    post_event(&RenderEvent::Ready);
    Ok(())
}

fn read_vec3(buffers: &js_sys::Array, idx: u32) -> Vec<[f32; 3]> {
    let arr = buffers.get(idx).unchecked_into::<js_sys::Uint8Array>();
    let bytes = arr.to_vec();
    bytes
        .chunks_exact(12)
        .map(|c| {
            [
                f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                f32::from_le_bytes([c[8], c[9], c[10], c[11]]),
            ]
        })
        .collect()
}

fn read_u32(buffers: &js_sys::Array, idx: u32) -> Vec<u32> {
    let arr = buffers.get(idx).unchecked_into::<js_sys::Uint8Array>();
    let bytes = arr.to_vec();
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}
