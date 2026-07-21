//! H6 — animated lights as sim-state (via the transform arena).
//!
//! A punctual light derives its world pose from a **bound transform**
//! (`lights.bind_transform` + `update_from_transforms`). Transforms are already
//! arena-backed (M2/M3) and H3 routes physics-updated transforms into the
//! per-frame dirty set that `update_from_transforms` consumes. So a physics
//! worker animates a light simply by moving its bound transform in the shared
//! arena — no separate lights buffer / dense-repack→stable-slot refactor is
//! needed (lights ride the transform arena exactly like glTF node-attached
//! lights). Zero `postMessage` on the hot path.
//!
//! The scene is a static ground slab under a black IBL/skybox, so the only
//! illumination is the point light — its bright spot sweeps across the ground
//! as the physics worker drives the light's transform.

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

    let on_msg = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        let data = e.data();
        let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_lights"), &data);
    });
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    let worker = crate::bootstrap::spawn_shared_worker_transfer(
        "lights-render",
        &offscreen,
        &transfer,
        on_msg.as_ref().unchecked_ref(),
    )?;
    on_msg.forget();
    crate::viewport::observe_resize(&canvas, &worker)?;
    tracing::info!("lights demo: spawned render worker");
    Ok(())
}

pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "lights-render" => render_main(payload),
        "lights-physics" => physics_main(payload),
        _ => Ok(()),
    }
}

fn render_main(payload: JsValue) -> Result<(), JsValue> {
    use awsm_renderer::web_global::navigator_gpu;
    use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};

    let canvas: web_sys::OffscreenCanvas = payload.unchecked_into();
    let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run_render(gpu_builder).await {
            tracing::error!("lights demo render: {err:?}");
        }
    });
    Ok(())
}

async fn run_render(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
) -> Result<(), JsValue> {
    use awsm_renderer::camera::CameraParams;
    use awsm_renderer::lights::Light;
    use awsm_renderer::materials::Material;
    use awsm_renderer::raw_mesh::RawMeshData;
    use awsm_renderer::transforms::Transform;
    use awsm_renderer::AwsmRendererBuilder;
    use awsm_renderer_core::command::color::Color;
    use awsm_renderer_core::cubemap::images::CubemapBitmapColors;
    use awsm_renderer_materials::pbr::PbrMaterial;
    use awsm_renderer_materials::MaterialAlphaMode;
    use awsm_renderer_meshgen::primitives::box_mesh;
    use glam::{Mat4, Vec3};

    // Black ambient/skybox so the point light is the ONLY illumination — its
    // moving spot is then unmistakable.
    let black = CubemapBitmapColors::all(Color::BLACK);
    let mut renderer = AwsmRendererBuilder::new(gpu_builder)
        .with_ibl_irradiance_colors(black.clone())
        .with_ibl_filtered_env_colors(black.clone())
        .with_skybox_colors(black)
        .build()
        .await
        .map_err(|e| JsValue::from_str(&format!("renderer build failed: {e}")))?;
    renderer.transforms.enable_shared_arena();

    // Static ground slab (diffuse, NOT emissive — lit only by the point light).
    let mut ground_mat = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
    ground_mat.base_color_factor = [0.85, 0.85, 0.88, 1.0];
    ground_mat.metallic_factor = 0.0;
    ground_mat.roughness_factor = 0.9;
    ground_mat.emissive_factor = [0.0, 0.0, 0.0];
    let ground_material = renderer.materials.insert(
        Material::Pbr(Box::new(ground_mat)),
        &renderer.textures,
        &renderer.dynamic_materials,
        &renderer.extras_pool,
    );
    let ground_mesh = box_mesh(Vec3::new(24.0, 0.5, 24.0));
    let ground_raw = RawMeshData {
        positions: ground_mesh.positions,
        normals: ground_mesh.normals,
        uv_sets: ground_mesh.uvs,
        colors: ground_mesh.colors,
        indices: ground_mesh.indices,
        ..Default::default()
    };
    let ground_tk = renderer.transforms.insert(
        Transform {
            translation: Vec3::new(0.0, -2.0, 0.0),
            ..Default::default()
        },
        None,
    );
    renderer
        .add_raw_mesh(ground_raw, ground_tk, ground_material)
        .map_err(|e| JsValue::from_str(&format!("ground add_raw_mesh: {e}")))?;

    // Point light bound to a transform node the physics worker will move.
    let light_key = renderer
        .insert_light(
            Light::Point {
                color: [1.0, 0.95, 0.85],
                intensity: 120.0,
                position: [0.0, 3.0, 0.0],
                range: 40.0,
            },
            None,
        )
        .map_err(|e| JsValue::from_str(&format!("insert_light: {e}")))?;
    let light_base = [0.0f32, 3.0, 0.0];
    let light_tk = renderer.transforms.insert(
        Transform {
            translation: Vec3::from_array(light_base),
            ..Default::default()
        },
        None,
    );
    renderer.lights.bind_transform(light_key, light_tk);

    renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| JsValue::from_str(&format!("commit_load: {e}")))?;
    // Establish initial world matrices (incl. the light node) in the arena.
    renderer.update_transforms();

    // Hand the physics worker the light node's slot binding.
    let dirty_addr = renderer
        .transforms
        .arena_dirty_words_addr()
        .ok_or_else(|| JsValue::from_str("arena not enabled"))?;
    let binding = renderer
        .transforms
        .arena_slot_binding(light_tk)
        .ok_or_else(|| JsValue::from_str("light slot binding missing"))?;
    let phys = js_sys::Array::new();
    phys.push(&JsValue::from_f64(dirty_addr as f64));
    phys.push(&JsValue::from_f64(binding.value_addr as f64));
    phys.push(&JsValue::from_f64(binding.version_addr as f64));
    phys.push(&JsValue::from_f64(binding.chunk as f64));
    phys.push(&JsValue::from_f64(light_base[1] as f64));
    let noop = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|_| {});
    crate::bootstrap::spawn_shared_worker_transfer(
        "lights-physics",
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

    *raf_init.borrow_mut() = Some(Closure::new(move || {
        let mut r = cell_loop.borrow_mut();
        let f = {
            let mut fb = frame.borrow_mut();
            *fb = fb.wrapping_add(1);
            *fb
        };
        let eye = Vec3::new(0.0, 9.0, 10.0);
        let view = Mat4::look_at_rh(eye, Vec3::new(0.0, -2.0, 0.0), Vec3::Y);
        // The renderer supplies the depth convention AND the live aspect,
        // so neither can drift from what it actually renders with.
        let mut camera_params = CameraParams::perspective(55.0_f32.to_radians(), 0.1, 100.0);
        camera_params.focus_distance = 12.0;
        let _ = r.set_camera(view, camera_params);
        r.update_transforms();
        if let Err(err) = r.render(None) {
            tracing::warn!("lights demo: render error: {err}");
        }
        if f == 3 {
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let msg = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&msg, &JsValue::from_str("ready"), &JsValue::TRUE);
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
    let dirty_addr = arr.get(0).as_f64().unwrap_or(0.0) as usize;
    let binding = SlotBinding {
        value_addr: arr.get(1).as_f64().unwrap_or(0.0) as usize,
        version_addr: arr.get(2).as_f64().unwrap_or(0.0) as usize,
        chunk: arr.get(3).as_f64().unwrap_or(0.0) as usize,
    };
    let base_y = arr.get(4).as_f64().unwrap_or(3.0) as f32;
    tracing::info!("lights physics worker: sweeping the light transform");

    let tick = Rc::new(RefCell::new(0u32));
    repeat_every(16, move || {
        let t = {
            let mut tb = tick.borrow_mut();
            *tb = tb.wrapping_add(1);
            *tb
        } as f32;
        // Sweep the light along X above the ground.
        let x = (t * 0.02).sin() * 8.0;
        let mut cols = [0f32; 16];
        cols[0] = 1.0;
        cols[5] = 1.0;
        cols[10] = 1.0;
        cols[15] = 1.0;
        cols[12] = x;
        cols[13] = base_y;
        cols[14] = 0.0;
        let bytes = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
        // SAFETY: binding points into shared memory; the owner keeps the light
        // node alive for the session.
        unsafe {
            foreign_write(binding, dirty_addr, bytes);
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
