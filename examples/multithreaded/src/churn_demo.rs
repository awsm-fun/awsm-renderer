//! H5 — live spawn / despawn topology transaction.
//!
//! Bodies are spawned and despawned continuously while the physics worker
//! writes the live ones. Topology is owner-only (the render worker), values are
//! foreign (the physics worker), and the hot write path stays free of
//! `postMessage` — only the rare bind/unbind/ack commands use it.
//!
//! The despawn is a **transaction** (decision C): the render worker posts
//! `unbind`, the physics worker stops writing that slot and posts `unbound`,
//! and only THEN does the render worker free the arena slot (via
//! `transforms.remove`). This guarantees a slot is never reused while a foreign
//! writer might still target it — so a freed-then-reused slot never delivers a
//! stale value.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use web_sys::js_sys;

const CAP: usize = 16; // max live bodies; the count oscillates 5..=CAP

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

    let on_msg = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        let data = e.data();
        let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_churn"), &data);
    });
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    let worker = crate::bootstrap::spawn_shared_worker_transfer(
        "churn-render",
        &offscreen,
        &transfer,
        on_msg.as_ref().unchecked_ref(),
    )?;
    on_msg.forget();
    crate::viewport::observe_resize(&canvas, &worker)?;
    tracing::info!("churn demo: spawned render worker");
    Ok(())
}

pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "churn-render" => render_main(payload),
        "churn-physics" => physics_main(payload),
        _ => Ok(()),
    }
}

// ───────────────────────── render worker (owner) ─────────────────────────

struct Body {
    transform_key: awsm_renderer::transforms::TransformKey,
    mesh_key: awsm_renderer::meshes::MeshKey,
    value_addr: usize,
}

struct ChurnState {
    renderer: awsm_renderer::AwsmRenderer,
    material: awsm_renderer::materials::MaterialKey,
    physics: web_sys::Worker,
    bodies: HashMap<u32, Body>,
    pending_unbind: std::collections::HashSet<u32>,
    next_id: u32,
    spawned: u32,
    despawned: u32,
    growing: bool,
    freed_addrs: std::collections::HashSet<usize>,
    reused_slots: u32,
    max_torn_accepted: usize,
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
        if let Err(err) = run_render(gpu_builder, canvas_handle).await {
            tracing::error!("churn demo render: {err:?}");
        }
    });
    Ok(())
}

async fn run_render(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    canvas: web_sys::OffscreenCanvas,
) -> Result<(), JsValue> {
    use awsm_renderer::camera::CameraMatrices;
    use awsm_renderer::materials::Material;
    use awsm_renderer::AwsmRendererBuilder;
    use awsm_renderer_materials::pbr::PbrMaterial;
    use awsm_renderer_materials::MaterialAlphaMode;
    use glam::{Mat4, Vec3};

    let mut renderer = AwsmRendererBuilder::new(gpu_builder)
        .build()
        .await
        .map_err(|e| JsValue::from_str(&format!("renderer build failed: {e}")))?;
    renderer.transforms.enable_shared_arena();
    // Commit once so the scene is committed (pipelines compiled) before bodies
    // start churning; subsequent add/remove re-commit lazily via render().
    renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| JsValue::from_str(&format!("commit_load failed: {e}")))?;

    let mut mat = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
    mat.base_color_factor = [0.4, 0.7, 1.0, 1.0];
    mat.emissive_factor = [1.5, 2.5, 3.5];
    let material = renderer.materials.insert(
        Material::Pbr(Box::new(mat)),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );

    let dirty_addr = renderer
        .transforms
        .arena_dirty_words_addr()
        .ok_or_else(|| JsValue::from_str("arena not enabled"))?;

    // Spawn the physics worker; hand it the dirty-bitmap address once.
    let state_holder: Rc<RefCell<Option<ChurnState>>> = Rc::new(RefCell::new(None));
    let state_for_ack = state_holder.clone();
    let on_phys =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            // physics → render ack: {kind:"unbound", id} — now safe to free the slot.
            let data = e.data();
            let kind = js_sys::Reflect::get(&data, &JsValue::from_str("kind"))
                .ok()
                .and_then(|v| v.as_string());
            if kind.as_deref() != Some("unbound") {
                return;
            }
            let id = js_sys::Reflect::get(&data, &JsValue::from_str("id"))
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(-1.0) as i64;
            if id < 0 {
                return;
            }
            if let Some(s) = state_for_ack.borrow_mut().as_mut() {
                free_body(s, id as u32);
            }
        });
    let phys_payload = js_sys::Object::new();
    set(
        &phys_payload,
        "dirty",
        &JsValue::from_f64(dirty_addr as f64),
    );
    let physics = crate::bootstrap::spawn_shared_worker_transfer(
        "churn-physics",
        &phys_payload,
        &js_sys::Array::new(),
        on_phys.as_ref().unchecked_ref(),
    )?;
    on_phys.forget();

    *state_holder.borrow_mut() = Some(ChurnState {
        renderer,
        material,
        physics,
        bodies: HashMap::new(),
        pending_unbind: std::collections::HashSet::new(),
        next_id: 0,
        spawned: 0,
        despawned: 0,
        growing: true,
        freed_addrs: std::collections::HashSet::new(),
        reused_slots: 0,
        max_torn_accepted: 0,
    });

    // Frame loop + a churn tick (spawn/despawn one body every few frames).
    #[allow(clippy::arc_with_non_send_sync)]
    let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let raf_init = raf.clone();
    let raf_run = raf.clone();
    let frame = Rc::new(RefCell::new(0u32));

    *raf_init.borrow_mut() = Some(Closure::new(move || {
        let f = {
            let mut fb = frame.borrow_mut();
            *fb = fb.wrapping_add(1);
            *fb
        };
        let mut guard = state_holder.borrow_mut();
        let Some(s) = guard.as_mut() else { return };

        // Churn: every 12 frames, spawn or despawn one body (oscillate).
        if f % 12 == 0 {
            churn_tick(s);
        }

        // Camera + render.
        let eye = Vec3::new(0.0, 0.0, 12.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let projection = Mat4::perspective_rh(
            60.0_f32.to_radians(),
            crate::viewport::aspect(&canvas),
            0.1,
            100.0,
        );
        let _ = s.renderer.update_camera(CameraMatrices {
            view,
            projection,
            position_world: eye,
            focus_distance: 10.0,
            aperture: 5.6,
            // Examples/model-tests stay forward-Z (features default; 003)
            reverse_z: false,
        });
        s.renderer.update_transforms();
        let torn = s.renderer.transforms.last_descend_stats().torn;
        if torn > s.max_torn_accepted {
            // `torn` here is detections, which self-heal; the arena guarantees
            // no torn value is *accepted*. We track it only as evidence of
            // real contention; the real correctness check is consistency.
        }
        let _ = s.renderer.render(None);

        if f % 30 == 0 {
            let live = s.bodies.len() as f64;
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let msg = js_sys::Object::new();
            set(&msg, "frame", &JsValue::from_f64(f as f64));
            set(&msg, "live", &JsValue::from_f64(live));
            set(&msg, "spawned", &JsValue::from_f64(s.spawned as f64));
            set(&msg, "despawned", &JsValue::from_f64(s.despawned as f64));
            set(
                &msg,
                "pendingUnbind",
                &JsValue::from_f64(s.pending_unbind.len() as f64),
            );
            set(
                &msg,
                "reusedSlots",
                &JsValue::from_f64(s.reused_slots as f64),
            );
            // Invariant: live == spawned - despawned.
            set(
                &msg,
                "invariantOk",
                &JsValue::from_bool(s.bodies.len() as u32 == s.spawned - s.despawned),
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
    Ok(())
}

/// Spawn or despawn one body, oscillating the live count between 5 and CAP.
fn churn_tick(s: &mut ChurnState) {
    let live = s.bodies.len();
    if s.growing && live >= CAP {
        s.growing = false;
    } else if !s.growing && live <= 5 {
        s.growing = true;
    }
    if s.growing {
        spawn_body(s);
    } else {
        request_despawn(s);
    }
}

fn spawn_body(s: &mut ChurnState) {
    use awsm_renderer::raw_mesh::RawMeshData;
    use awsm_renderer::transforms::Transform;
    use awsm_renderer_meshgen::primitives::box_mesh;
    use glam::Vec3;

    let id = s.next_id;
    s.next_id += 1;
    // Spread bodies across a grid-ish region by id.
    let col = (id % 5) as f32 - 2.0;
    let row = ((id / 5) % 5) as f32 - 2.0;
    let base = [col * 1.6, row * 1.6, 0.0];
    let tk = s.renderer.transforms.insert(
        Transform {
            translation: Vec3::from_array(base),
            ..Default::default()
        },
        None,
    );
    let mesh = box_mesh(Vec3::splat(0.7));
    let raw = RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uv_sets: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
        ..Default::default()
    };
    let mesh_key = match s.renderer.add_raw_mesh(raw, tk, s.material) {
        Ok(k) => k,
        Err(err) => {
            tracing::warn!("churn spawn: add_raw_mesh failed: {err}");
            s.renderer.transforms.remove(tk);
            return;
        }
    };
    let binding = match s.renderer.transforms.arena_slot_binding(tk) {
        Some(b) => b,
        None => return,
    };
    if s.freed_addrs.remove(&binding.value_addr) {
        s.reused_slots += 1; // this arena slot was previously freed → reuse
    }
    // Topology command (NOT hot path): bind the physics worker to this slot.
    let msg = js_sys::Object::new();
    set(&msg, "kind", &JsValue::from_str("bind"));
    set(&msg, "id", &JsValue::from_f64(id as f64));
    set(&msg, "value", &JsValue::from_f64(binding.value_addr as f64));
    set(
        &msg,
        "version",
        &JsValue::from_f64(binding.version_addr as f64),
    );
    set(&msg, "chunk", &JsValue::from_f64(binding.chunk as f64));
    set(&msg, "bx", &JsValue::from_f64(base[0] as f64));
    set(&msg, "by", &JsValue::from_f64(base[1] as f64));
    set(&msg, "bz", &JsValue::from_f64(base[2] as f64));
    let _ = s.physics.post_message(&msg);
    s.bodies.insert(
        id,
        Body {
            transform_key: tk,
            mesh_key,
            value_addr: binding.value_addr,
        },
    );
    s.spawned += 1;
}

/// Begin a despawn transaction: ask physics to stop writing this slot. The slot
/// is freed only on the `unbound` ack (see [`free_body`]).
fn request_despawn(s: &mut ChurnState) {
    let Some(&id) = s
        .bodies
        .keys()
        .find(|id| !s.pending_unbind.contains(id))
        .copied()
        .as_ref()
    else {
        return;
    };
    s.pending_unbind.insert(id);
    let msg = js_sys::Object::new();
    set(&msg, "kind", &JsValue::from_str("unbind"));
    set(&msg, "id", &JsValue::from_f64(id as f64));
    let _ = s.physics.post_message(&msg);
}

/// Physics acked the unbind — now it is safe to free the slot (no foreign
/// writer targets it). This is the transaction's commit point.
fn free_body(s: &mut ChurnState, id: u32) {
    s.pending_unbind.remove(&id);
    if let Some(body) = s.bodies.remove(&id) {
        s.renderer.remove_mesh(body.mesh_key);
        s.renderer.transforms.remove(body.transform_key);
        s.freed_addrs.insert(body.value_addr);
        s.despawned += 1;
    }
}

// ───────────────────────── physics worker (writer) ─────────────────────────

#[derive(Clone, Copy)]
struct PhysBody {
    binding: awsm_renderer::buffer::shared_arena::SlotBinding,
    base: [f32; 3],
}

fn physics_main(payload: JsValue) -> Result<(), JsValue> {
    use awsm_renderer::buffer::shared_arena::{foreign_write, SlotBinding};

    let dirty_addr = js_sys::Reflect::get(&payload, &JsValue::from_str("dirty"))?
        .as_f64()
        .unwrap_or(0.0) as usize;

    let bodies: Rc<RefCell<HashMap<u32, PhysBody>>> = Rc::new(RefCell::new(HashMap::new()));

    // bind/unbind command handler (topology channel). On unbind, drop the slot
    // and ACK so the owner can safely free it.
    let bodies_cmd = bodies.clone();
    let on_cmd =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            let data = e.data();
            let kind = js_sys::Reflect::get(&data, &JsValue::from_str("kind"))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            let id = js_sys::Reflect::get(&data, &JsValue::from_str("id"))
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(-1.0) as i64;
            if id < 0 {
                return;
            }
            let id = id as u32;
            match kind.as_str() {
                "bind" => {
                    let get = |k: &str| {
                        js_sys::Reflect::get(&data, &JsValue::from_str(k))
                            .ok()
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0)
                    };
                    let binding = SlotBinding {
                        value_addr: get("value") as usize,
                        version_addr: get("version") as usize,
                        chunk: get("chunk") as usize,
                    };
                    let base = [get("bx") as f32, get("by") as f32, get("bz") as f32];
                    bodies_cmd
                        .borrow_mut()
                        .insert(id, PhysBody { binding, base });
                }
                "unbind" => {
                    bodies_cmd.borrow_mut().remove(&id);
                    // ACK: the owner frees the slot only after this.
                    let scope =
                        js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
                    let ack = js_sys::Object::new();
                    set(&ack, "kind", &JsValue::from_str("unbound"));
                    set(&ack, "id", &JsValue::from_f64(id as f64));
                    let _ = scope.post_message(&ack);
                }
                _ => {}
            }
        });
    js_sys::global()
        .unchecked_into::<web_sys::DedicatedWorkerGlobalScope>()
        .set_onmessage(Some(on_cmd.as_ref().unchecked_ref()));
    on_cmd.forget();

    // Hot loop: write every currently-bound slot. Zero postMessage here.
    let tick = Rc::new(RefCell::new(0u32));
    repeat_every(16, move || {
        let t = {
            let mut tb = tick.borrow_mut();
            *tb = tb.wrapping_add(1);
            *tb
        } as f32;
        for (id, b) in bodies.borrow().iter() {
            let bob = (t * 0.06 + *id as f32 * 0.5).sin() * 0.5;
            let mut cols = [0f32; 16];
            cols[0] = 1.0;
            cols[5] = 1.0;
            cols[10] = 1.0;
            cols[15] = 1.0;
            cols[12] = b.base[0];
            cols[13] = b.base[1] + bob;
            cols[14] = b.base[2];
            let bytes = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
            // SAFETY: the binding addresses point into shared memory; the owner
            // does not free this slot until it acks our `unbound` for it.
            unsafe {
                foreign_write(b.binding, dirty_addr, bytes);
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
