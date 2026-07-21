//! M2 — worker-hosted renderer over the **shared-memory transform arena**.
//!
//! The render worker builds the full `AwsmRenderer` against a transferred
//! `OffscreenCanvas`, switches its transform store into shared sim-state
//! mode ([`awsm_renderer::transforms::Transforms::enable_shared_arena`]),
//! and draws a box. The box's world matrix now lives as a semantic 64-byte
//! value in shared linear memory; the render worker's per-frame descent
//! packs it to the 112-byte GPU layout (model + inverse-transpose normal)
//! and uploads via the existing path — so the scene renders **identically**
//! to the single-threaded / non-arena build, just sourced from the arena.
//!
//! `?demo=render` (default). `?arena=0` runs the same scene on the classic
//! non-arena path for an A/B visual comparison; `?spin=1` animates it
//! (kept static by default so screenshots are deterministic). M3 adds the
//! physics worker that writes those same arena slots.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use web_sys::js_sys;

/// Main thread: transfer the canvas and spawn the render worker.
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
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok();
    let arena = params
        .as_ref()
        .and_then(|p| p.get("arena"))
        .map(|v| v != "0")
        .unwrap_or(true);
    let spin = params
        .as_ref()
        .and_then(|p| p.get("spin"))
        .map(|v| v == "1")
        .unwrap_or(false);

    let payload = js_sys::Object::new();
    set(&payload, "canvas", &offscreen);
    set(&payload, "arena", &JsValue::from_bool(arena));
    set(&payload, "spin", &JsValue::from_bool(spin));

    let on_msg = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        let data = e.data();
        let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_render"), &data);
        tracing::info!("render demo: worker reported {:?}", data);
    });
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    let worker = crate::bootstrap::spawn_shared_worker_transfer(
        "render",
        &payload,
        &transfer,
        on_msg.as_ref().unchecked_ref(),
    )?;
    on_msg.forget();
    crate::viewport::observe_resize(&canvas, &worker)?;
    tracing::info!("render demo: spawned render worker (arena={arena}, spin={spin})");
    Ok(())
}

/// Worker-side dispatch. `physics` is M3; here only `render`.
pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "render" => render_main(payload),
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
    let use_arena = js_sys::Reflect::get(&payload, &JsValue::from_str("arena"))?
        .as_bool()
        .unwrap_or(true);
    let spin = js_sys::Reflect::get(&payload, &JsValue::from_str("spin"))?
        .as_bool()
        .unwrap_or(false);

    let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    // Request the adapter's max limits — the renderer's advanced passes
    // (material prep, edge resolve) need >8 storage buffers / >16 sampled
    // textures per stage, same as the editor / model-viewer.
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run_renderer(gpu_builder, use_arena, spin, canvas_handle).await {
            tracing::error!("render demo: {err:?}");
        }
    });
    Ok(())
}

async fn run_renderer(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    use_arena: bool,
    spin: bool,
    canvas: web_sys::OffscreenCanvas,
) -> Result<(), JsValue> {
    use awsm_renderer::camera::CameraMatrices;
    use awsm_renderer::materials::Material;
    use awsm_renderer::raw_mesh::RawMeshData;
    use awsm_renderer::transforms::Transform;
    use awsm_renderer::AwsmRendererBuilder;
    use awsm_renderer_materials::pbr::PbrMaterial;
    use awsm_renderer_materials::MaterialAlphaMode;
    use awsm_renderer_meshgen::primitives::box_mesh;
    use glam::{Mat4, Quat, Vec3};

    let mut renderer = AwsmRendererBuilder::new(gpu_builder)
        .build()
        .await
        .map_err(|e| JsValue::from_str(&format!("renderer build failed: {e}")))?;

    // M2: route world transforms through the shared-memory arena.
    if use_arena {
        renderer.transforms.enable_shared_arena();
    }
    tracing::info!(
        "render demo: renderer built (shared transforms = {})",
        renderer.transforms.is_shared()
    );

    let mesh = box_mesh(Vec3::splat(1.0));
    let raw = RawMeshData {
        positions: mesh.positions,
        normals: mesh.normals,
        uv_sets: mesh.uvs,
        colors: mesh.colors,
        indices: mesh.indices,
        ..Default::default()
    };
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
    // A fixed oblique orientation so a 3D box (and thus the normal matrix /
    // shading) is visible — and so an arena-vs-direct screenshot is a
    // deterministic A/B match.
    let base_rotation = Quat::from_rotation_y(0.6) * Quat::from_rotation_x(0.3);
    let transform_key = renderer.transforms.insert(
        Transform {
            translation: Vec3::new(0.0, 0.0, -3.0),
            rotation: base_rotation,
            ..Default::default()
        },
        None,
    );
    renderer
        .add_raw_mesh(raw, transform_key, material_key)
        .map_err(|e| JsValue::from_str(&format!("add_raw_mesh failed: {e}")))?;
    renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| JsValue::from_str(&format!("commit_load failed: {e}")))?;

    #[allow(clippy::arc_with_non_send_sync)]
    let cell = Rc::new(RefCell::new(renderer));
    #[allow(clippy::arc_with_non_send_sync)]
    let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    let raf_init = raf.clone();
    let raf_run = raf.clone();
    let cell_loop = cell.clone();
    let frame = Rc::new(RefCell::new(0u32));
    let reported = Rc::new(RefCell::new(false));

    *raf_init.borrow_mut() = Some(Closure::new(move || {
        let mut r = cell_loop.borrow_mut();
        let f = {
            let mut fb = frame.borrow_mut();
            *fb = fb.wrapping_add(1);
            *fb
        };
        if spin {
            let angle = f as f32 * 0.01;
            let _ = r.transforms.set_local(
                transform_key,
                Transform {
                    translation: Vec3::new(0.0, 0.0, -3.0),
                    rotation: base_rotation * Quat::from_rotation_y(angle),
                    ..Default::default()
                },
            );
        }
        let view = Mat4::look_at_rh(Vec3::new(0.0, 1.5, 3.0), Vec3::new(0.0, 0.0, -3.0), Vec3::Y);
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
            position_world: Vec3::new(0.0, 1.5, 3.0),
            focus_distance: 10.0,
            aperture: 5.6,
            reverse_z: convention.reverse_z,
            near: 0.1,
            far: 100.0,
        });
        r.update_transforms();
        if let Err(err) = r.render(None) {
            tracing::warn!("render demo: render error: {err}");
        }
        // Report readiness once, after the first successful frame.
        if f == 2 && !*reported.borrow() {
            *reported.borrow_mut() = true;
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let msg = js_sys::Object::new();
            set(&msg, "ready", &JsValue::from_bool(true));
            set(
                &msg,
                "shared",
                &JsValue::from_bool(r.transforms.is_shared()),
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

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}
