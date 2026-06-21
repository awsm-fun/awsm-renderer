//! M3 — physics worker writes transforms → objects move (hot-path proof).
//!
//! The render worker hosts the renderer with the shared transform arena
//! (M2), spawns N boxes, and hands the **physics** worker the raw slot
//! bindings for each box's world matrix — once, at spawn (the "topology
//! command channel": one `postMessage`). From then on the physics worker
//! integrates motion and writes world `Mat4`s straight into shared linear
//! memory via [`awsm_renderer::buffer::shared_arena::foreign_write`] (seqlock
//! bump + chunk dirty bit). The render worker's per-frame `update_world`
//! descent picks those writes up, packs 64 B → 112 B, and uploads.
//!
//! **Zero `postMessage` on the hot path** — the physics worker never posts
//! after setup; coordination is native atomics in shared memory.
//!
//! Only the first half of the bodies move ("movers"); the rest stay static,
//! so the descent's `updated` count tracks the movers, not the total slot
//! count (`?stress=N` to scale, default 25).

use std::cell::RefCell;
use std::rc::Rc;

use awsm_renderer::buffer::shared_arena::foreign_write;
use awsm_renderer::buffer::shared_arena::SlotBinding;
use wasm_bindgen::prelude::*;
use web_sys::js_sys;

/// Main thread: transfer the canvas + spawn the render worker.
pub fn start_main() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let canvas: web_sys::HtmlCanvasElement = document
        .get_element_by_id("canvas")
        .ok_or_else(|| JsValue::from_str("no #canvas"))?
        .unchecked_into();
    let offscreen = canvas.transfer_control_to_offscreen()?;

    let search = window.location().search().unwrap_or_default();
    let count = web_sys::UrlSearchParams::new_with_str(&search)
        .ok()
        .and_then(|p| p.get("stress"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(25)
        .max(2);

    let payload = js_sys::Object::new();
    set(&payload, "canvas", &offscreen);
    set(&payload, "count", &JsValue::from_f64(count as f64));

    let on_msg = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        let data = e.data();
        let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_motion"), &data);
    });
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    crate::bootstrap::spawn_shared_worker_transfer(
        "motion-render",
        &payload,
        &transfer,
        on_msg.as_ref().unchecked_ref(),
    )?;
    on_msg.forget();
    tracing::info!("motion demo: spawned render worker ({count} bodies)");
    Ok(())
}

pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "motion-render" => render_main(payload),
        "motion-physics" => physics_main(payload),
        _ => Ok(()),
    }
}

fn render_main(payload: JsValue) -> Result<(), JsValue> {
    use awsm_renderer::web_global::navigator_gpu;
    use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};

    let canvas: web_sys::OffscreenCanvas =
        js_sys::Reflect::get(&payload, &JsValue::from_str("canvas"))?.unchecked_into();
    let count = js_sys::Reflect::get(&payload, &JsValue::from_str("count"))?
        .as_f64()
        .unwrap_or(25.0) as usize;

    let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run_render(gpu_builder, count).await {
            tracing::error!("motion demo render: {err:?}");
        }
    });
    Ok(())
}

/// Grid layout: place body `i` of `count` on a roughly-square grid in the
/// z=0 plane, centred at the origin.
fn body_base(i: usize, count: usize) -> [f32; 3] {
    let cols = (count as f64).sqrt().ceil() as usize;
    let rows = count.div_ceil(cols);
    let cx = (cols.saturating_sub(1)) as f32 * 0.5;
    let cy = (rows.saturating_sub(1)) as f32 * 0.5;
    let col = (i % cols) as f32;
    let row = (i / cols) as f32;
    [(col - cx) * 1.4, (row - cy) * 1.4, 0.0]
}

async fn run_render(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    count: usize,
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
    renderer.transforms.enable_shared_arena();

    let mut mat = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
    mat.base_color_factor = [0.4, 0.7, 1.0, 1.0];
    mat.emissive_factor = [1.5, 3.0, 4.5];
    let material_key = renderer.materials.insert(
        Material::Pbr(Box::new(mat)),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );

    let mut transform_keys = Vec::with_capacity(count);
    for i in 0..count {
        let base = body_base(i, count);
        let tk = renderer.transforms.insert(
            Transform {
                translation: Vec3::from_array(base),
                ..Default::default()
            },
            None,
        );
        let mesh = box_mesh(Vec3::splat(0.8));
        let raw = RawMeshData {
            positions: mesh.positions,
            normals: mesh.normals,
            uv_sets: mesh.uvs,
            colors: mesh.colors,
            indices: mesh.indices,
            ..Default::default()
        };
        renderer
            .add_raw_mesh(raw, tk, material_key)
            .map_err(|e| JsValue::from_str(&format!("add_raw_mesh failed: {e}")))?;
        transform_keys.push(tk);
    }
    renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| JsValue::from_str(&format!("commit_load failed: {e}")))?;

    // Establish initial world matrices in the arena (one walk + descent).
    renderer.update_transforms();

    // Hand the physics worker each body's slot binding (the topology
    // command channel — one postMessage at spawn). The first half are
    // movers; the rest static.
    let movers = (count / 2).max(1);
    let dirty_addr = renderer
        .transforms
        .arena_dirty_words_addr()
        .ok_or_else(|| JsValue::from_str("arena not enabled"))?;
    let phys_payload = js_sys::Array::new();
    phys_payload.push(&JsValue::from_f64(count as f64));
    phys_payload.push(&JsValue::from_f64(movers as f64));
    phys_payload.push(&JsValue::from_f64(dirty_addr as f64));
    for (i, tk) in transform_keys.iter().enumerate() {
        let b = renderer
            .transforms
            .arena_slot_binding(*tk)
            .ok_or_else(|| JsValue::from_str("missing slot binding"))?;
        let base = body_base(i, count);
        phys_payload.push(&JsValue::from_f64(b.value_addr as f64));
        phys_payload.push(&JsValue::from_f64(b.version_addr as f64));
        phys_payload.push(&JsValue::from_f64(b.chunk as f64));
        phys_payload.push(&JsValue::from_f64(base[0] as f64));
        phys_payload.push(&JsValue::from_f64(base[1] as f64));
        phys_payload.push(&JsValue::from_f64(base[2] as f64));
    }

    // Count any messages the physics worker posts back (must stay 0 — the
    // hot path is shared memory, not postMessage).
    let phys_msgs = Rc::new(RefCell::new(0u32));
    let phys_msgs_cb = phys_msgs.clone();
    let on_phys = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
        *phys_msgs_cb.borrow_mut() += 1;
    });
    crate::bootstrap::spawn_shared_worker_transfer(
        "motion-physics",
        &phys_payload,
        &js_sys::Array::new(),
        on_phys.as_ref().unchecked_ref(),
    )?;
    on_phys.forget();

    // Frame loop: descend (picks up physics writes) + render.
    #[allow(clippy::arc_with_non_send_sync)]
    let cell = Rc::new(RefCell::new(renderer));
    #[allow(clippy::arc_with_non_send_sync)]
    let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let raf_init = raf.clone();
    let raf_run = raf.clone();
    let cell_loop = cell.clone();
    let frame = Rc::new(RefCell::new(0u32));
    // Running maxima — the per-frame `updated` count fluctuates with
    // render/physics interleave, so report the peak: it equals the mover
    // count, proving descent work tracks movers, not the total slot count.
    let max_updated = Rc::new(RefCell::new(0usize));
    let max_chunks = Rc::new(RefCell::new(0usize));

    *raf_init.borrow_mut() = Some(Closure::new(move || {
        let mut r = cell_loop.borrow_mut();
        let f = {
            let mut fb = frame.borrow_mut();
            *fb = fb.wrapping_add(1);
            *fb
        };
        let eye = Vec3::new(0.0, 0.0, 9.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let projection = Mat4::perspective_rh(60.0_f32.to_radians(), 800.0 / 600.0, 0.1, 100.0);
        let _ = r.update_camera(CameraMatrices {
            view,
            projection,
            position_world: eye,
            focus_distance: 10.0,
            aperture: 5.6,
        });
        r.update_transforms();
        if let Err(err) = r.render(None) {
            tracing::warn!("motion demo: render error: {err}");
        }
        let stats = r.transforms.last_descend_stats();
        {
            let mu = &mut *max_updated.borrow_mut();
            if stats.updated > *mu {
                *mu = stats.updated;
            }
            let mc = &mut *max_chunks.borrow_mut();
            if stats.chunks > *mc {
                *mc = stats.chunks;
            }
        }
        // Report a snapshot every 30 frames (NOT per frame, and never on the
        // sim hot path — this is observability only).
        if f % 30 == 0 {
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let msg = js_sys::Object::new();
            set(&msg, "ready", &JsValue::from_bool(true));
            set(&msg, "frame", &JsValue::from_f64(f as f64));
            set(&msg, "total", &JsValue::from_f64(count as f64));
            set(&msg, "movers", &JsValue::from_f64(movers as f64));
            set(
                &msg,
                "lastUpdated",
                &JsValue::from_f64(stats.updated as f64),
            );
            set(
                &msg,
                "maxUpdated",
                &JsValue::from_f64(*max_updated.borrow() as f64),
            );
            set(
                &msg,
                "maxChunks",
                &JsValue::from_f64(*max_chunks.borrow() as f64),
            );
            set(&msg, "lastChunks", &JsValue::from_f64(stats.chunks as f64));
            set(&msg, "lastTorn", &JsValue::from_f64(stats.torn as f64));
            set(
                &msg,
                "physicsMessages",
                &JsValue::from_f64(*phys_msgs.borrow() as f64),
            );
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
    let arr: js_sys::Array = payload.unchecked_into();
    let count = arr.get(0).as_f64().unwrap_or(0.0) as usize;
    let movers = arr.get(1).as_f64().unwrap_or(0.0) as usize;
    let dirty_addr = arr.get(2).as_f64().unwrap_or(0.0) as usize;
    let mut bindings = Vec::with_capacity(count);
    let mut bases = Vec::with_capacity(count);
    for i in 0..count {
        let base = 3 + i * 6;
        bindings.push(SlotBinding {
            value_addr: arr.get(base as u32).as_f64().unwrap_or(0.0) as usize,
            version_addr: arr.get((base + 1) as u32).as_f64().unwrap_or(0.0) as usize,
            chunk: arr.get((base + 2) as u32).as_f64().unwrap_or(0.0) as usize,
        });
        bases.push([
            arr.get((base + 3) as u32).as_f64().unwrap_or(0.0) as f32,
            arr.get((base + 4) as u32).as_f64().unwrap_or(0.0) as f32,
            arr.get((base + 5) as u32).as_f64().unwrap_or(0.0) as f32,
        ]);
    }
    tracing::info!("motion physics worker: {count} bodies ({movers} movers), integrating motion");

    let tick = Rc::new(RefCell::new(0u32));
    repeat_every(16, move || {
        let t = {
            let mut tb = tick.borrow_mut();
            *tb = tb.wrapping_add(1);
            *tb
        } as f32;
        // Integrate motion for the movers and write world matrices into the
        // shared arena. No postMessage — pure shared-memory writes.
        for i in 0..movers {
            let base = bases[i];
            let bob = (t * 0.06 + i as f32 * 0.5).sin() * 0.6;
            // Column-major translation matrix (glam Mat4 layout).
            let mut cols = [0f32; 16];
            cols[0] = 1.0;
            cols[5] = 1.0;
            cols[10] = 1.0;
            cols[15] = 1.0;
            cols[12] = base[0];
            cols[13] = base[1] + bob;
            cols[14] = base[2];
            let bytes = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
            // SAFETY: bindings/dirty_addr point into the shared memory both
            // workers attached to; the owner arena outlives this worker.
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
