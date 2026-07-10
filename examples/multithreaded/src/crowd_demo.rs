//! M4 — instance transforms + attributes over shared memory (crowd/particle
//! stress).
//!
//! One instanced mesh with N instances. The render worker owns the topology
//! (instance count) and creates two shared arenas — one for per-instance
//! world `Mat4`s (stride 64 = `INSTANCE_TRANSFORM_BYTE_SIZE`), one for
//! per-instance [`InstanceAttr`](awsm_renderer::instances::InstanceAttr)
//! (stride 16) — then hands the physics worker the slot bindings once.
//!
//! The physics worker integrates the crowd and writes per-instance world
//! matrices + attributes straight into shared memory
//! ([`foreign_write`](awsm_renderer::buffer::shared_arena::foreign_write)) —
//! zero `postMessage` on the hot path, mirroring the particle-sim's
//! `transform_write_all` / `attribute_write_all` cadence. The render worker
//! descends both arenas each frame and hands the contiguous mirrors straight
//! to [`Instances::transform_write_all_bytes`](awsm_renderer::instances::Instances::transform_write_all_bytes)
//! / `attribute_write_all_bytes` (GPU-ready bytes — no `Transform`
//! round-trip), then uploads.

use std::cell::RefCell;
use std::rc::Rc;

use awsm_renderer::buffer::shared_arena::{foreign_write, SharedArena, SlotBinding};
use wasm_bindgen::prelude::*;
use web_sys::js_sys;

const TF_STRIDE: usize = 64; // INSTANCE_TRANSFORM_BYTE_SIZE (one Mat4)
const ATTR_STRIDE: usize = 16; // InstanceAttr::BYTE_SIZE

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

    let search = window.location().search().unwrap_or_default();
    let count = web_sys::UrlSearchParams::new_with_str(&search)
        .ok()
        .and_then(|p| p.get("stress"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(64)
        .max(2);

    let payload = js_sys::Object::new();
    set(&payload, "canvas", &offscreen);
    set(&payload, "count", &JsValue::from_f64(count as f64));

    let on_msg = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        let data = e.data();
        let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_crowd"), &data);
    });
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    let worker = crate::bootstrap::spawn_shared_worker_transfer(
        "crowd-render",
        &payload,
        &transfer,
        on_msg.as_ref().unchecked_ref(),
    )?;
    on_msg.forget();
    crate::viewport::observe_resize(&canvas, &worker)?;
    tracing::info!("crowd demo: spawned render worker ({count} instances)");
    Ok(())
}

pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "crowd-render" => render_main(payload),
        "crowd-physics" => physics_main(payload),
        _ => Ok(()),
    }
}

/// Grid position for instance `i` of `count`.
fn instance_base(i: usize, count: usize) -> [f32; 3] {
    let cols = (count as f64).sqrt().ceil() as usize;
    let rows = count.div_ceil(cols);
    let cx = (cols.saturating_sub(1)) as f32 * 0.5;
    let cy = (rows.saturating_sub(1)) as f32 * 0.5;
    let col = (i % cols) as f32;
    let row = (i / cols) as f32;
    [(col - cx) * 1.2, (row - cy) * 1.2, 0.0]
}

/// Column-major translation `Mat4` as 64 raw bytes.
fn translate_bytes(x: f32, y: f32, z: f32) -> [u8; TF_STRIDE] {
    let mut cols = [0f32; 16];
    cols[0] = 1.0;
    cols[5] = 1.0;
    cols[10] = 1.0;
    cols[15] = 1.0;
    cols[12] = x;
    cols[13] = y;
    cols[14] = z;
    let mut out = [0u8; TF_STRIDE];
    out.copy_from_slice(unsafe {
        std::slice::from_raw_parts(cols.as_ptr() as *const u8, TF_STRIDE)
    });
    out
}

/// `InstanceAttr` (color_packed:u32, size:f32, alpha:f32, _pad:u32) as 16
/// raw bytes.
fn attr_bytes(color_packed: u32, size: f32, alpha: f32) -> [u8; ATTR_STRIDE] {
    let mut out = [0u8; ATTR_STRIDE];
    out[0..4].copy_from_slice(&color_packed.to_le_bytes());
    out[4..8].copy_from_slice(&size.to_le_bytes());
    out[8..12].copy_from_slice(&alpha.to_le_bytes());
    out
}

fn pack_rgba(r: f32, g: f32, b: f32, a: f32) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    q(r) | (q(g) << 8) | (q(b) << 16) | (q(a) << 24)
}

fn render_main(payload: JsValue) -> Result<(), JsValue> {
    use awsm_renderer::web_global::navigator_gpu;
    use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};

    let canvas: web_sys::OffscreenCanvas =
        js_sys::Reflect::get(&payload, &JsValue::from_str("canvas"))?.unchecked_into();
    let canvas_handle = canvas.clone();
    crate::viewport::install_worker_resize(&canvas_handle);
    let count = js_sys::Reflect::get(&payload, &JsValue::from_str("count"))?
        .as_f64()
        .unwrap_or(64.0) as usize;

    let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run_render(gpu_builder, count, canvas_handle).await {
            tracing::error!("crowd demo render: {err:?}");
        }
    });
    Ok(())
}

async fn run_render(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    count: usize,
    canvas: web_sys::OffscreenCanvas,
) -> Result<(), JsValue> {
    use awsm_renderer::camera::CameraMatrices;
    use awsm_renderer::instances::InstanceAttr;
    use awsm_renderer::materials::Material;
    use awsm_renderer::raw_mesh::RawMeshData;
    use awsm_renderer::transforms::Transform;
    use awsm_renderer::AwsmRendererBuilder;
    use awsm_renderer_materials::pbr::PbrMaterial;
    use awsm_renderer_materials::MaterialAlphaMode;
    use awsm_renderer_meshgen::primitives::box_mesh;
    use glam::{Mat4, Vec3};

    let mut renderer = AwsmRendererBuilder::new(gpu_builder)
        .build()
        .await
        .map_err(|e| JsValue::from_str(&format!("renderer build failed: {e}")))?;

    let mut mat = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
    mat.base_color_factor = [1.0, 1.0, 1.0, 1.0];
    mat.emissive_factor = [2.0, 2.0, 2.0];
    let material_key = renderer.materials.insert(
        Material::Pbr(Box::new(mat)),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );

    // One instanced mesh; its node transform stays at the origin (instances
    // carry world positions).
    let node_tk = renderer.transforms.insert(Transform::IDENTITY, None);
    let mesh = box_mesh(Vec3::splat(0.5));
    let raw = RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uv_sets: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
        ..Default::default()
    };
    let mesh_key = renderer
        .add_raw_mesh(raw, node_tk, material_key)
        .map_err(|e| JsValue::from_str(&format!("add_raw_mesh failed: {e}")))?;

    // Topology (owner-side): allocate the N instances once.
    let initial_transforms: Vec<Transform> = (0..count)
        .map(|i| {
            let b = instance_base(i, count);
            Transform {
                translation: Vec3::from_array(b),
                ..Default::default()
            }
        })
        .collect();
    let initial_attrs: Vec<InstanceAttr> = (0..count)
        .map(|_| InstanceAttr::from_rgba_alpha_size([0.4, 0.7, 1.0, 1.0], 1.0, 1.0))
        .collect();
    renderer
        .enable_mesh_instancing_opaque(mesh_key, &initial_transforms)
        .map_err(|e| JsValue::from_str(&format!("enable instancing failed: {e}")))?;
    renderer
        .set_mesh_instance_attrs(node_tk, &initial_attrs)
        .map_err(|e| JsValue::from_str(&format!("set attrs failed: {e}")))?;
    renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| JsValue::from_str(&format!("commit_load failed: {e}")))?;

    // Two shared arenas (instance transforms + attributes), seeded with the
    // initial values so the first descent is consistent.
    let mut tf_arena = SharedArena::new(TF_STRIDE, count.max(1), 4);
    let mut attr_arena = SharedArena::new(ATTR_STRIDE, count.max(1), 4);
    for i in 0..count {
        let s = tf_arena.allocate();
        let b = instance_base(i, count);
        tf_arena.write_value(s, &translate_bytes(b[0], b[1], b[2]));
        let sa = attr_arena.allocate();
        attr_arena.write_value(sa, &attr_bytes(pack_rgba(0.4, 0.7, 1.0, 1.0), 1.0, 1.0));
    }

    // Hand the physics worker both arenas' bindings (one postMessage).
    let phys_payload = js_sys::Array::new();
    phys_payload.push(&JsValue::from_f64(count as f64));
    phys_payload.push(&JsValue::from_f64(tf_arena.dirty_words_addr() as f64));
    phys_payload.push(&JsValue::from_f64(attr_arena.dirty_words_addr() as f64));
    for i in 0..count {
        let tb: SlotBinding = tf_arena.slot_binding(i);
        let ab: SlotBinding = attr_arena.slot_binding(i);
        let base = instance_base(i, count);
        phys_payload.push(&JsValue::from_f64(tb.value_addr as f64));
        phys_payload.push(&JsValue::from_f64(tb.version_addr as f64));
        phys_payload.push(&JsValue::from_f64(tb.chunk as f64));
        phys_payload.push(&JsValue::from_f64(ab.value_addr as f64));
        phys_payload.push(&JsValue::from_f64(ab.version_addr as f64));
        phys_payload.push(&JsValue::from_f64(ab.chunk as f64));
        phys_payload.push(&JsValue::from_f64(base[0] as f64));
        phys_payload.push(&JsValue::from_f64(base[1] as f64));
        phys_payload.push(&JsValue::from_f64(base[2] as f64));
    }
    let phys_msgs = Rc::new(RefCell::new(0u32));
    let phys_msgs_cb = phys_msgs.clone();
    let on_phys = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
        *phys_msgs_cb.borrow_mut() += 1;
    });
    crate::bootstrap::spawn_shared_worker_transfer(
        "crowd-physics",
        &phys_payload,
        &js_sys::Array::new(),
        on_phys.as_ref().unchecked_ref(),
    )?;
    on_phys.forget();

    #[allow(clippy::arc_with_non_send_sync)]
    let cell = Rc::new(RefCell::new(renderer));
    #[allow(clippy::arc_with_non_send_sync)]
    let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let raf_init = raf.clone();
    let raf_run = raf.clone();
    let cell_loop = cell.clone();
    let frame = Rc::new(RefCell::new(0u32));
    let tf_arena = Rc::new(RefCell::new(tf_arena));
    let attr_arena = Rc::new(RefCell::new(attr_arena));

    *raf_init.borrow_mut() = Some(Closure::new(move || {
        let mut r = cell_loop.borrow_mut();
        let f = {
            let mut fb = frame.borrow_mut();
            *fb = fb.wrapping_add(1);
            *fb
        };
        // Pull physics writes out of shared memory and into the instance
        // buffers (GPU-ready bytes, no Transform round-trip).
        {
            let mut tfa = tf_arena.borrow_mut();
            let res = tfa.descend();
            if res.updated > 0 || f <= 2 {
                let bytes = &tfa.mirror()[..count * TF_STRIDE];
                if let Err(err) = r.instances.transform_write_all_bytes(node_tk, bytes) {
                    tracing::warn!("crowd: transform_write_all_bytes: {err}");
                }
            }
        }
        {
            let mut aa = attr_arena.borrow_mut();
            let res = aa.descend();
            if res.updated > 0 || f <= 2 {
                let bytes = &aa.mirror()[..count * ATTR_STRIDE];
                if let Err(err) = r.instances.attribute_write_all_bytes(node_tk, bytes) {
                    tracing::warn!("crowd: attribute_write_all_bytes: {err}");
                }
            }
        }

        let eye = Vec3::new(0.0, 0.0, (count as f32).sqrt() * 2.2 + 4.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let projection = Mat4::perspective_rh(
            60.0_f32.to_radians(),
            crate::viewport::aspect(&canvas),
            0.1,
            200.0,
        );
        let _ = r.update_camera(CameraMatrices {
            view,
            projection,
            position_world: eye,
            focus_distance: 10.0,
            aperture: 5.6,
            // Examples/model-tests stay forward-Z (features default; 003)
            reverse_z: false,
            near: 0.1,
            far: 200.0,
        });
        r.update_transforms();
        if let Err(err) = r.render(None) {
            tracing::warn!("crowd demo: render error: {err}");
        }
        if f % 30 == 0 {
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let msg = js_sys::Object::new();
            set(&msg, "ready", &JsValue::from_bool(true));
            set(&msg, "frame", &JsValue::from_f64(f as f64));
            set(&msg, "instances", &JsValue::from_f64(count as f64));
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
    let tf_dirty = arr.get(1).as_f64().unwrap_or(0.0) as usize;
    let attr_dirty = arr.get(2).as_f64().unwrap_or(0.0) as usize;
    let mut tf_bind = Vec::with_capacity(count);
    let mut attr_bind = Vec::with_capacity(count);
    let mut bases = Vec::with_capacity(count);
    for i in 0..count {
        let o = 3 + i * 9;
        tf_bind.push(SlotBinding {
            value_addr: arr.get(o as u32).as_f64().unwrap_or(0.0) as usize,
            version_addr: arr.get((o + 1) as u32).as_f64().unwrap_or(0.0) as usize,
            chunk: arr.get((o + 2) as u32).as_f64().unwrap_or(0.0) as usize,
        });
        attr_bind.push(SlotBinding {
            value_addr: arr.get((o + 3) as u32).as_f64().unwrap_or(0.0) as usize,
            version_addr: arr.get((o + 4) as u32).as_f64().unwrap_or(0.0) as usize,
            chunk: arr.get((o + 5) as u32).as_f64().unwrap_or(0.0) as usize,
        });
        bases.push([
            arr.get((o + 6) as u32).as_f64().unwrap_or(0.0) as f32,
            arr.get((o + 7) as u32).as_f64().unwrap_or(0.0) as f32,
            arr.get((o + 8) as u32).as_f64().unwrap_or(0.0) as f32,
        ]);
    }
    tracing::info!("crowd physics worker: {count} instances, integrating motion + color");

    let tick = Rc::new(RefCell::new(0u32));
    repeat_every(16, move || {
        let t = {
            let mut tb = tick.borrow_mut();
            *tb = tb.wrapping_add(1);
            *tb
        } as f32;
        for i in 0..count {
            let base = bases[i];
            let phase = i as f32 * 0.35;
            let bob = (t * 0.05 + phase).sin() * 0.5;
            let bytes = translate_bytes(base[0], base[1] + bob, base[2]);
            unsafe {
                foreign_write(tf_bind[i], tf_dirty, &bytes);
            }
            // Pulse color so the attribute path is exercised too.
            let r = (t * 0.03 + phase).sin() * 0.5 + 0.5;
            let g = (t * 0.03 + phase + 2.0).sin() * 0.5 + 0.5;
            let b = (t * 0.03 + phase + 4.0).sin() * 0.5 + 0.5;
            let a = attr_bytes(pack_rgba(r, g, b, 1.0), 1.0, 1.0);
            unsafe {
                foreign_write(attr_bind[i], attr_dirty, &a);
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
