//! H6 (closed) / H7 — animated **skin deform** as sim-state, via a real rigged
//! glTF driven through the transform arena.
//!
//! Skin joints ARE transform nodes, and joint matrices are recomputed from the
//! per-frame dirty-transform set (`meshes::update_world` →
//! `skins.update_transforms`, meshes.rs:1855) — the same path H3 feeds with
//! physics-updated transforms. So the physics worker deforms a skinned mesh by
//! writing **joint** world matrices into the shared arena: no new renderer
//! code, zero `postMessage` on the hot path.
//!
//! This loads CesiumMan.glb (a real rig with correctly-authored joints / IBMs /
//! weights — unlike a hand-rolled skinned mesh), reads the joint transform keys
//! from the glTF populate context, and has the physics worker rotate each joint
//! around its bind pose (`bind_world × R(t)`), flexing the figure.

use std::cell::RefCell;
use std::rc::Rc;

use awsm_renderer::buffer::shared_arena::{foreign_write, SlotBinding};
use wasm_bindgen::prelude::*;
use web_sys::js_sys;

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
    let origin = window.location().origin().unwrap_or_default();

    let payload = js_sys::Object::new();
    set(&payload, "canvas", &offscreen);
    set(
        &payload,
        "url",
        &JsValue::from_str(&format!("{origin}/CesiumMan.glb")),
    );

    let on_msg = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        let data = e.data();
        let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_skin"), &data);
    });
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    let worker = crate::bootstrap::spawn_shared_worker_transfer(
        "skin-render",
        &payload,
        &transfer,
        on_msg.as_ref().unchecked_ref(),
    )?;
    on_msg.forget();
    crate::viewport::observe_resize(&canvas, &worker)?;
    tracing::info!("skin demo: spawned render worker");
    Ok(())
}

pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "skin-render" => render_main(payload),
        "skin-physics" => physics_main(payload),
        _ => Ok(()),
    }
}

fn render_main(payload: JsValue) -> Result<(), JsValue> {
    use awsm_renderer::web_global::navigator_gpu;
    use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};

    let canvas: web_sys::OffscreenCanvas =
        js_sys::Reflect::get(&payload, &JsValue::from_str("canvas"))?.unchecked_into();
    let canvas_handle = canvas.clone();
    let url = js_sys::Reflect::get(&payload, &JsValue::from_str("url"))?
        .as_string()
        .unwrap_or_default();
    let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run_render(gpu_builder, canvas_handle, url).await {
            tracing::error!("skin demo render: {err:?}");
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let msg = js_sys::Object::new();
            set(&msg, "error", &JsValue::from_str(&format!("{err:?}")));
            let _ = scope.post_message(&msg);
        }
    });
    Ok(())
}

async fn run_render(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    canvas: web_sys::OffscreenCanvas,
    url: String,
) -> Result<(), JsValue> {
    use awsm_renderer::camera::CameraMatrices;
    use awsm_renderer::AwsmRendererBuilder;
    use awsm_renderer_gltf::{AwsmRendererGltfExt, GltfLoader};
    use glam::{Mat4, Vec3};

    let mut renderer = AwsmRendererBuilder::new(gpu_builder)
        .build()
        .await
        .map_err(|e| JsValue::from_str(&format!("build: {e}")))?;
    // Shared mode BEFORE populate so the glTF's nodes (incl. skin joints) get
    // arena slots — making the joints foreign-writable.
    renderer.transforms.enable_shared_arena();

    let bytes = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| JsValue::from_str(&format!("fetch: {e}")))?
        .binary()
        .await
        .map_err(|e| JsValue::from_str(&format!("read: {e}")))?;
    let loader = GltfLoader::from_glb_bytes(&bytes)
        .await
        .map_err(|e| JsValue::from_str(&format!("glb: {e}")))?;
    let data = loader
        .into_data(None)
        .map_err(|e| JsValue::from_str(&format!("into_data: {e}")))?;
    let ctx = renderer
        .populate_gltf(data, None)
        .await
        .map_err(|e| JsValue::from_str(&format!("populate: {e}")))?;
    renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| JsValue::from_str(&format!("commit: {e}")))?;
    renderer.update_transforms();

    // Collect the skin joints + each joint's bind world matrix.
    let joints: Vec<awsm_renderer::transforms::TransformKey> = ctx
        .transform_is_joint
        .lock()
        .unwrap()
        .iter()
        .copied()
        .collect();
    let dirty_addr = renderer
        .transforms
        .arena_dirty_words_addr()
        .ok_or_else(|| JsValue::from_str("arena not enabled"))?;
    let phys = js_sys::Array::new();
    phys.push(&JsValue::from_f64(dirty_addr as f64));
    phys.push(&JsValue::from_f64(joints.len() as f64));
    for jk in &joints {
        let binding = renderer
            .transforms
            .arena_slot_binding(*jk)
            .ok_or_else(|| JsValue::from_str("joint slot binding missing"))?;
        let bind_world = renderer
            .transforms
            .get_world(*jk)
            .copied()
            .unwrap_or(Mat4::IDENTITY);
        phys.push(&JsValue::from_f64(binding.value_addr as f64));
        phys.push(&JsValue::from_f64(binding.version_addr as f64));
        phys.push(&JsValue::from_f64(binding.chunk as f64));
        for v in bind_world.to_cols_array() {
            phys.push(&JsValue::from_f64(v as f64));
        }
    }
    tracing::info!("skin demo: {} joints, spawning physics", joints.len());

    let noop = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|_| {});
    crate::bootstrap::spawn_shared_worker_transfer(
        "skin-physics",
        &phys,
        &js_sys::Array::new(),
        noop.as_ref().unchecked_ref(),
    )?;
    noop.forget();

    #[allow(clippy::arc_with_non_send_sync)]
    let cell = Rc::new(RefCell::new(renderer));
    #[allow(clippy::arc_with_non_send_sync)]
    let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let raf_init = raf.clone();
    let raf_run = raf.clone();
    let cell_loop = cell.clone();
    let frame = Rc::new(RefCell::new(0u32));
    let joint_count = joints.len();

    *raf_init.borrow_mut() = Some(Closure::new(move || {
        let mut r = cell_loop.borrow_mut();
        let f = {
            let mut fb = frame.borrow_mut();
            *fb = fb.wrapping_add(1);
            *fb
        };
        // CesiumMan stands ~1.6 units tall around y≈0.8; orbit to frame it.
        let yaw = f as f32 * 0.004;
        let eye = Vec3::new(yaw.sin() * 3.5, 1.0, yaw.cos() * 3.5);
        let view = Mat4::look_at_rh(eye, Vec3::new(0.0, 0.8, 0.0), Vec3::Y);
        // One source for the projection AND the reverse_z flag below, so
        // the two cannot drift — the renderer owns the convention.
        let convention = r.features.depth();
        let projection = convention.perspective(
            55.0_f32.to_radians(),
            crate::viewport::aspect(&canvas),
            0.05,
            100.0,
        );
        let _ = r.update_camera(CameraMatrices {
            view,
            projection,
            position_world: eye,
            focus_distance: 4.0,
            aperture: 5.6,
            reverse_z: convention.reverse_z,
            near: 0.05,
            far: 100.0,
        });
        r.update_transforms();
        if let Err(err) = r.render(None) {
            tracing::warn!("skin demo: render error: {err}");
        }
        if f == 3 {
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let msg = js_sys::Object::new();
            set(&msg, "ready", &JsValue::TRUE);
            set(&msg, "joints", &JsValue::from_f64(joint_count as f64));
            let _ = scope.post_message(&msg);
        }
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

fn physics_main(payload: JsValue) -> Result<(), JsValue> {
    use glam::Mat4;
    let arr: js_sys::Array = payload.unchecked_into();
    let dirty_addr = arr.get(0).as_f64().unwrap_or(0.0) as usize;
    let count = arr.get(1).as_f64().unwrap_or(0.0) as usize;
    let mut bindings = Vec::with_capacity(count);
    let mut binds = Vec::with_capacity(count);
    for i in 0..count {
        let o = 2 + i * 19;
        bindings.push(SlotBinding {
            value_addr: arr.get(o as u32).as_f64().unwrap_or(0.0) as usize,
            version_addr: arr.get((o + 1) as u32).as_f64().unwrap_or(0.0) as usize,
            chunk: arr.get((o + 2) as u32).as_f64().unwrap_or(0.0) as usize,
        });
        let mut cols = [0f32; 16];
        for (k, c) in cols.iter_mut().enumerate() {
            *c = arr.get((o + 3 + k) as u32).as_f64().unwrap_or(0.0) as f32;
        }
        binds.push(Mat4::from_cols_array(&cols));
    }
    tracing::info!("skin physics worker: flexing {count} joints");

    let tick = Rc::new(RefCell::new(0u32));
    repeat_every(16, move || {
        let t = {
            let mut tb = tick.borrow_mut();
            *tb = tb.wrapping_add(1);
            *tb
        } as f32;
        for i in 0..count {
            // Rotate each joint around its bind pose — the figure flexes.
            let angle = (t * 0.04 + i as f32 * 0.7).sin() * 0.5;
            let world = binds[i] * Mat4::from_rotation_z(angle);
            let cols = world.to_cols_array();
            let bytes = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
            // SAFETY: joint bindings point into shared memory owned by the
            // render worker for the session.
            unsafe {
                foreign_write(bindings[i], dirty_addr, bytes);
            }
        }
    })?;
    Ok(())
}

fn repeat_every<F: FnMut() + 'static>(ms: i32, mut f: F) -> Result<(), JsValue> {
    let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
    let holder: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let holder_run = holder.clone();
    let scope_run = scope.clone();
    *holder.borrow_mut() = Some(Closure::new(move || {
        f();
        if let Some(cb) = holder_run.borrow().as_ref() {
            let _ = scope_run.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                ms,
            );
        }
    }));
    if let Some(cb) = holder.borrow().as_ref() {
        scope.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            ms,
        )?;
    }
    std::mem::forget(holder);
    Ok(())
}

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}
