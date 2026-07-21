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

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use awsm_renderer::buffer::shared_arena::foreign_write;
use awsm_renderer::buffer::shared_arena::SlotBinding;
use wasm_bindgen::prelude::*;
use web_sys::js_sys;

/// Main thread: transfer the canvas + spawn the render worker.
///
/// Resilience scope (harden-memory B1): a lost **GPU device** is recovered
/// **in place** by the render worker (see `arm_recovery`) — the worker lives, so
/// it's seamless + leak-free. A render-worker **process death** (a Rust panic →
/// `abort`, or a browser OOM-kill) is treated as **catastrophic and not
/// recovered**: a killed wasm thread can't run destructors, so respawning it
/// would orphan its shared-memory allocations (an unbounded per-crash leak), and
/// a panic is unrecoverable by design anyway. Device-loss is the common,
/// recoverable failure; that's the one we handle.
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
        .unwrap_or(25)
        .max(2);

    let payload = js_sys::Object::new();
    set(&payload, "canvas", &offscreen);
    set(&payload, "count", &JsValue::from_f64(count as f64));

    let on_msg = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        let _ = js_sys::Reflect::set(
            &js_sys::global(),
            &JsValue::from_str("__mt_motion"),
            &e.data(),
        );
    });
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    let worker = crate::bootstrap::spawn_shared_worker_transfer(
        "motion-render",
        &payload,
        &transfer,
        on_msg.as_ref().unchecked_ref(),
    )?;
    on_msg.forget();
    crate::viewport::observe_resize(&canvas, &worker)?;
    // Test seam (gated): expose the render-worker handle so a chrome-devtools
    // `evaluate_script` on the PAGE can post `{kind:"__mt_test_lose_device"}` to
    // force a `device.destroy()` inside the worker — the device lives in the
    // worker scope and isn't otherwise reachable from the page. Never ships.
    #[cfg(any(debug_assertions, feature = "harden-diag"))]
    {
        let _ = js_sys::Reflect::set(
            &js_sys::global(),
            &JsValue::from_str("__mt_motion_worker"),
            &worker,
        );
    }
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
    let canvas_handle = canvas.clone();
    crate::viewport::install_worker_resize(&canvas_handle);
    let count = js_sys::Reflect::get(&payload, &JsValue::from_str("count"))?
        .as_f64()
        .unwrap_or(25.0) as usize;

    let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run_render(gpu_builder, count, canvas_handle).await {
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

/// Build a fresh renderer on `gpu_builder`'s device + (re)construct the box
/// scene from the **source-of-truth** (this construction code — the renderer
/// drops geometry CPU mirrors after upload, so reload-from-source is the only
/// recovery path; see harden-memory.md B1a). Returns the renderer + its
/// transform keys (the physics-worker topology). Shared by the initial boot and
/// the cold device-loss recovery path — identical scene either way.
async fn build_renderer_and_scene(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    count: usize,
) -> Result<
    (
        awsm_renderer::AwsmRenderer,
        Vec<awsm_renderer::transforms::TransformKey>,
    ),
    JsValue,
> {
    use awsm_renderer::materials::Material;
    use awsm_renderer::raw_mesh::RawMeshData;
    use awsm_renderer::transforms::Transform;
    use awsm_renderer::AwsmRendererBuilder;
    use awsm_renderer_materials::pbr::PbrMaterial;
    use awsm_renderer_materials::MaterialAlphaMode;
    use awsm_renderer_meshgen::primitives::box_mesh;
    use glam::Vec3;

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
    Ok((renderer, transform_keys))
}

/// Hand the physics worker each body's **fresh** slot binding (the topology
/// command channel — one postMessage at spawn) and spawn it. The first half are
/// movers; the rest static. Re-run verbatim on recovery against the new arena's
/// bindings so the movers resume. Returns the spawned worker so the caller can
/// `terminate()` it before the arena it writes into is freed.
fn spawn_physics(
    renderer: &awsm_renderer::AwsmRenderer,
    count: usize,
    transform_keys: &[awsm_renderer::transforms::TransformKey],
    phys_msgs: Rc<RefCell<u32>>,
) -> Result<web_sys::Worker, JsValue> {
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
    let on_phys = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
        *phys_msgs.borrow_mut() += 1;
    });
    let worker = crate::bootstrap::spawn_shared_worker_transfer(
        "motion-physics",
        &phys_payload,
        &js_sys::Array::new(),
        on_phys.as_ref().unchecked_ref(),
    )?;
    on_phys.forget();
    Ok(worker)
}

/// Everything the cold device-loss recovery path needs, bundled so the loss
/// callback + `recover` pass it around as one cheap `Clone` (all `Rc`/handles).
/// NOT touched on the per-frame hot path — only on `.lost`.
#[derive(Clone)]
struct RecoveryCtx {
    cell: Rc<RefCell<awsm_renderer::AwsmRenderer>>,
    physics: Rc<RefCell<Option<web_sys::Worker>>>,
    phys_msgs: Rc<RefCell<u32>>,
    /// Set while a rebuild is in flight; the frame loop's single per-frame check
    /// reads this to skip frames against the dead device (and avoid racing the
    /// `cell` swap). The ONLY per-frame cost recovery adds.
    recovering: Rc<Cell<bool>>,
    count: usize,
    canvas: web_sys::OffscreenCanvas,
}

/// Arm the device-loss **action seam** on the renderer's current device:
/// **event-driven** (no per-frame poll). On loss it kicks `recover` directly.
/// Re-armed after every recovery so a second loss recovers too.
fn arm_recovery(ctx: RecoveryCtx) {
    let ctx2 = ctx.clone();
    ctx.cell.borrow().gpu.on_device_lost(move |reason| {
        tracing::warn!("motion demo: GPU device lost ({reason}) — starting recovery");
        if ctx2.recovering.get() {
            return; // already recovering
        }
        ctx2.recovering.set(true);
        wasm_bindgen_futures::spawn_local(recover(ctx2.clone()));
    });
}

/// Cold device-loss recovery (B1a, reload-from-source): rebuild the renderer on
/// a **fresh** device + replay the box scene, then re-hand the physics worker
/// the new arena bindings. The old renderer is dropped (frees its dead GPU
/// handles + old arena). No page reload; movers resume.
async fn recover(ctx: RecoveryCtx) {
    use awsm_renderer::web_global::navigator_gpu;
    use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};

    // Stop the old physics worker BEFORE the old arena is freed — its writes
    // target addresses inside the old shared-arena allocation.
    if let Some(w) = ctx.physics.borrow_mut().take() {
        w.terminate();
    }

    let result = async {
        let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("recover: no navigator.gpu"))?;
        let gpu_builder =
            AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, ctx.canvas.clone())
                .with_device_request_limits(DeviceRequestLimits::max_all());
        let (new_renderer, tks) = build_renderer_and_scene(gpu_builder, ctx.count).await?;
        // Swap in the fresh renderer (drops the old → frees dead handles + old
        // arena), re-arm the loss seam, re-spawn physics on the NEW bindings.
        *ctx.cell.borrow_mut() = new_renderer;
        arm_recovery(ctx.clone());
        let w = spawn_physics(&ctx.cell.borrow(), ctx.count, &tks, ctx.phys_msgs.clone())?;
        *ctx.physics.borrow_mut() = Some(w);
        Ok::<(), JsValue>(())
    }
    .await;

    match result {
        Ok(()) => tracing::info!("motion demo: GPU device recovered — rendering resumed"),
        Err(err) => tracing::error!("motion demo: recovery FAILED: {err:?}"),
    }
    ctx.recovering.set(false);
}

/// Combined worker `onmessage`: resize forwarding + the **gated test seam** that
/// forces `device.destroy()` so the device-loss recovery repro is drivable from
/// the page (the device lives in this worker scope). Replaces the resize-only
/// handler `install_worker_resize` would set.
fn install_motion_onmessage(
    canvas: &web_sys::OffscreenCanvas,
    cell: &Rc<RefCell<awsm_renderer::AwsmRenderer>>,
) {
    let canvas = canvas.clone();
    #[allow(unused_variables)]
    let cell = cell.clone();
    let cb = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
        let data = e.data();
        if crate::viewport::try_apply_resize(&canvas, &data).is_some() {
            return;
        }
        #[cfg(any(debug_assertions, feature = "harden-diag"))]
        {
            let kind = js_sys::Reflect::get(&data, &JsValue::from_str("kind"))
                .ok()
                .and_then(|v| v.as_string());
            if kind.as_deref() == Some("__mt_test_lose_device") {
                tracing::warn!("motion demo: TEST seam — forcing device.destroy()");
                cell.borrow().gpu.device.destroy();
            }
        }
    });
    js_sys::global()
        .unchecked_into::<web_sys::DedicatedWorkerGlobalScope>()
        .set_onmessage(Some(cb.as_ref().unchecked_ref()));
    cb.forget();
}

async fn run_render(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    count: usize,
    canvas: web_sys::OffscreenCanvas,
) -> Result<(), JsValue> {
    use awsm_renderer::camera::CameraMatrices;
    use glam::{Mat4, Vec3};

    let (renderer, transform_keys) = build_renderer_and_scene(gpu_builder, count).await?;
    let movers = (count / 2).max(1);

    // Shared state across the frame loop + the cold recovery path.
    #[allow(clippy::arc_with_non_send_sync)]
    let cell = Rc::new(RefCell::new(renderer));
    let phys_msgs = Rc::new(RefCell::new(0u32));
    let ctx = RecoveryCtx {
        cell: cell.clone(),
        physics: Rc::new(RefCell::new(None)),
        phys_msgs: phys_msgs.clone(),
        recovering: Rc::new(Cell::new(false)),
        count,
        canvas: canvas.clone(),
    };

    // Spawn the physics worker against the live arena bindings.
    *ctx.physics.borrow_mut() = Some(spawn_physics(
        &cell.borrow(),
        count,
        &transform_keys,
        phys_msgs.clone(),
    )?);
    // Arm the device-loss action seam (event-driven) + the combined resize/test
    // onmessage.
    arm_recovery(ctx.clone());
    install_motion_onmessage(&canvas, &cell);

    // Frame loop: descend (picks up physics writes) + render.
    #[allow(clippy::arc_with_non_send_sync)]
    let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let raf_init = raf.clone();
    let raf_run = raf.clone();
    let cell_loop = cell.clone();
    let recovering = ctx.recovering.clone();
    let frame = Rc::new(RefCell::new(0u32));
    // Running maxima — the per-frame `updated` count fluctuates with
    // render/physics interleave, so report the peak: it equals the mover
    // count, proving descent work tracks movers, not the total slot count.
    let max_updated = Rc::new(RefCell::new(0usize));
    let max_chunks = Rc::new(RefCell::new(0usize));
    // H3 culling proof: track the min/max frustum-visible mesh count. Body 0 is
    // a "traveler" that sweeps far off-screen and back; if its CPU world_aabb
    // tracks the physics position, the spatial index excludes it when it leaves
    // the frustum → min_visible < total. With stale bounds it would always be
    // counted (min_visible == total).
    let min_visible = Rc::new(RefCell::new(usize::MAX));
    let max_visible = Rc::new(RefCell::new(0usize));

    *raf_init.borrow_mut() = Some(Closure::new(move || {
        // The ONLY per-frame cost recovery adds: one `Cell<bool>` read. While a
        // device-loss rebuild is in flight the device is dead — skip the frame
        // (and don't race the `cell` swap). False in steady state.
        if recovering.get() {
            if let Some(cb) = raf_run.borrow().as_ref() {
                let _ =
                    awsm_renderer::web_global::request_animation_frame(cb.as_ref().unchecked_ref());
            }
            return;
        }
        let mut r = cell_loop.borrow_mut();
        let f = {
            let mut fb = frame.borrow_mut();
            *fb = fb.wrapping_add(1);
            *fb
        };
        let eye = Vec3::new(0.0, 0.0, 9.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        // One source for the projection AND the reverse_z flag below, so
        // the two cannot drift — the renderer owns the convention.
        let convention = r.features.depth();
        let projection = convention.perspective(
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
            reverse_z: convention.reverse_z,
            near: 0.1,
            far: 100.0,
        });
        r.update_transforms();
        // Probe the spatial index AFTER the descent has refreshed sim-owned
        // bounds — this is what frustum culling / shadows / picking consult.
        {
            let frustum =
                awsm_renderer::frustum::Frustum::from_view_projection(projection * view, false);
            let visible = r.scene_spatial.query_frustum_raw(&frustum).count();
            let mn = &mut *min_visible.borrow_mut();
            if visible < *mn {
                *mn = visible;
            }
            let mx = &mut *max_visible.borrow_mut();
            if visible > *mx {
                *mx = visible;
            }
        }
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
            set(
                &msg,
                "minVisible",
                &JsValue::from_f64(*min_visible.borrow() as f64),
            );
            set(
                &msg,
                "maxVisible",
                &JsValue::from_f64(*max_visible.borrow() as f64),
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
            // Body 0 is the "traveler": it sweeps far along X, fully off-screen
            // and back, so the H3 frustum-culling probe can confirm its CPU
            // world_aabb tracks the physics position. Others bob in place.
            let (dx, dy) = if i == 0 {
                ((t * 0.02).sin() * 16.0, 0.0)
            } else {
                (0.0, (t * 0.06 + i as f32 * 0.5).sin() * 0.6)
            };
            // Column-major translation matrix (glam Mat4 layout).
            let mut cols = [0f32; 16];
            cols[0] = 1.0;
            cols[5] = 1.0;
            cols[10] = 1.0;
            cols[15] = 1.0;
            cols[12] = base[0] + dx;
            cols[13] = base[1] + dy;
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
