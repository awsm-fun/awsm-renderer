//! Game-API parity — load a real **exported scene** in the render worker and
//! drive it at runtime, exactly as a shipped game would.
//!
//! The typical player flow is NOT glTF-direct (that's for the editor /
//! model-viewer): the editor exports a `Scene`, and the game loads it with
//! `awsm_scene_loader::load_scene_for_player`. This demo does that **inside the
//! render worker** — proving the threaded build can do what single-threaded
//! does — then exercises the runtime control surface that makes it a game:
//!
//! - **Load** a real editor-exported scene FILE (`scene_fixture/scene.toml`,
//!   bundled same-origin) over the player loader — fetched in the worker,
//!   deserialized with `scene_from_toml`; its environment / lights / meshes all
//!   materialize. Falls back to an equivalent in-code `demo_scene()` if the
//!   fetch fails (B4).
//! - **Add a light at runtime** (`insert_light`) bound to a fresh transform.
//! - **Hook it to physics** via the shared transform arena — a physics worker
//!   sweeps the runtime light with zero `postMessage` on the hot path.
//!
//! Per-frame node motion uses Layer 2 (the arena), control uses Layer 1 — the
//! same split the rest of the example establishes.

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

    let payload = js_sys::Object::new();
    set(&payload, "canvas", &offscreen);
    // The render worker has a `blob:` base URL, so relative fetches can't
    // resolve — pass the page origin so it can build the absolute scene.toml URL.
    let origin = window.location().origin().unwrap_or_default();
    set(&payload, "origin", &JsValue::from_str(&origin));

    let on_msg = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
        let data = e.data();
        let _ = js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__mt_scene"), &data);
    });
    let transfer = js_sys::Array::new();
    transfer.push(&offscreen);
    let worker = crate::bootstrap::spawn_shared_worker_transfer(
        "scene-render",
        &payload,
        &transfer,
        on_msg.as_ref().unchecked_ref(),
    )?;
    on_msg.forget();
    crate::viewport::observe_resize(&canvas, &worker)?;
    tracing::info!("scene demo: spawned render worker");
    Ok(())
}

pub fn worker_dispatch(role: &str, payload: JsValue) -> Result<(), JsValue> {
    match role {
        "scene-render" => render_main(payload),
        "scene-physics" => physics_main(payload),
        _ => Ok(()),
    }
}

fn render_main(payload: JsValue) -> Result<(), JsValue> {
    use awsm_renderer::web_global::navigator_gpu;
    use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};

    let canvas: web_sys::OffscreenCanvas =
        js_sys::Reflect::get(&payload, &JsValue::from_str("canvas"))?.unchecked_into();
    let canvas_handle = canvas.clone();
    let origin = js_sys::Reflect::get(&payload, &JsValue::from_str("origin"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let gpu = navigator_gpu().ok_or_else(|| JsValue::from_str("worker: no navigator.gpu"))?;
    let gpu_builder = AwsmRendererWebGpuBuilder::new_with_offscreen_canvas(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run_render(gpu_builder, canvas_handle, origin).await {
            tracing::error!("scene demo render: {err:?}");
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let msg = js_sys::Object::new();
            set(&msg, "error", &JsValue::from_str(&format!("{err:?}")));
            let _ = scope.post_message(&msg);
        }
    });
    Ok(())
}

/// Fetch a same-origin player-bundle `scene.toml` and deserialize it to a
/// runtime [`Scene`](awsm_scene::Scene). Same-origin is required under
/// COOP/COEP (`require-corp` blocks cross-origin fetches).
async fn fetch_scene_file(url: &str) -> Result<awsm_scene::Scene, String> {
    let text = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?
        .text()
        .await
        .map_err(|e| format!("read {url}: {e}"))?;
    awsm_scene::project_dir::scene_from_toml(&text).map_err(|e| format!("parse {url}: {e}"))
}

async fn run_render(
    gpu_builder: awsm_renderer_core::renderer::AwsmRendererWebGpuBuilder,
    canvas: web_sys::OffscreenCanvas,
    origin: String,
) -> Result<(), JsValue> {
    use awsm_renderer::camera::CameraMatrices;
    use awsm_renderer::lights::Light;
    use awsm_renderer::transforms::Transform;
    use awsm_renderer::AwsmRendererBuilder;
    use glam::{Mat4, Vec3};

    let mut renderer = AwsmRendererBuilder::new(gpu_builder)
        .build()
        .await
        .map_err(|e| JsValue::from_str(&format!("build: {e}")))?;
    // Shared mode BEFORE the load so the scene's nodes get arena slots — any of
    // them can then be foreign-driven by a physics worker.
    renderer.transforms.enable_shared_arena();

    // ── Load a real editor-exported Scene FILE via the PLAYER path (B4) ──────
    // The shipped flow: the editor's `export_player_bundle` emits `scene.toml`
    // (a serialized runtime `awsm_scene::Scene` from `project_to_scene`); a game
    // fetches it and runs `load_scene_for_player`. We do exactly that IN THE
    // WORKER — fetch the bundled `scene.toml` same-origin, deserialize with
    // `awsm_scene::project_dir::scene_from_toml`, and load it (materials,
    // primitive meshes, lights, environment, the commit transaction). Per-frame
    // state then rides Layer 2 (the arena) below. If the fetch/parse fails we
    // fall back to the equivalent in-code `demo_scene()` so the demo still runs.
    // (A scene with external assets would pass a same-origin async `SceneAssets`
    // fetcher instead of the empty map; this primitives-only fixture needs none.)
    let scene_url = format!("{}/scene.toml", origin.trim_end_matches('/'));
    let scene = match fetch_scene_file(&scene_url).await {
        Ok(s) => {
            tracing::info!(
                "scene demo: loaded bundled scene FILE {scene_url} ({} nodes)",
                s.nodes.len()
            );
            s
        }
        Err(e) => {
            tracing::warn!(
                "scene demo: scene.toml fetch/parse failed ({e}); using in-code demo_scene()"
            );
            demo_scene()
        }
    };
    let assets: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
    let loaded = awsm_scene_loader::load_scene_for_player(&mut renderer, &scene, &assets, |_| {})
        .await
        .map_err(|e| JsValue::from_str(&format!("load_scene_for_player: {e}")))?;
    let node_count = loaded.nodes.len();
    let prefab_count = loaded.prefabs.len();

    // ── Runtime op: add a light + hook it to physics via the arena ──────────
    let light_key = renderer
        .insert_light(
            Light::Point {
                color: [1.0, 0.6, 0.3],
                intensity: 80.0,
                position: [0.0, 4.0, 0.0],
                range: 40.0,
            },
            None,
        )
        .map_err(|e| JsValue::from_str(&format!("insert_light: {e}")))?;
    let light_tk = renderer.transforms.insert(
        Transform {
            translation: Vec3::new(0.0, 4.0, 0.0),
            ..Default::default()
        },
        None,
    );
    renderer.lights.bind_transform(light_key, light_tk);

    renderer
        .commit_load(|_| {})
        .await
        .map_err(|e| JsValue::from_str(&format!("commit_load: {e}")))?;
    renderer.update_transforms();

    // Frame the loaded scene from its world bounds.
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for node in renderer.scene_spatial.iter_all() {
        lo = lo.min(node.aabb.min);
        hi = hi.max(node.aabb.max);
    }
    if !lo.is_finite() {
        lo = Vec3::splat(-4.0);
        hi = Vec3::splat(4.0);
    }
    let center = (lo + hi) * 0.5;
    let radius = ((hi - lo).length() * 0.5).max(1.0);

    // Hand the runtime light's transform slot to a physics worker.
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
    phys.push(&JsValue::from_f64(center.x as f64));
    phys.push(&JsValue::from_f64((hi.y + radius * 0.5) as f64));
    phys.push(&JsValue::from_f64(center.z as f64));
    phys.push(&JsValue::from_f64(radius as f64));
    let noop = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|_| {});
    crate::bootstrap::spawn_shared_worker_transfer(
        "scene-physics",
        &phys,
        &js_sys::Array::new(),
        noop.as_ref().unchecked_ref(),
    )?;
    noop.forget();
    tracing::info!("scene demo: loaded {node_count} nodes, {prefab_count} prefabs");

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
        let yaw = f as f32 * 0.004;
        let eye = center
            + Vec3::new(
                yaw.sin() * radius * 2.2,
                radius * 1.1,
                yaw.cos() * radius * 2.2,
            );
        let view = Mat4::look_at_rh(eye, center, Vec3::Y);
        let projection = Mat4::perspective_rh(
            55.0_f32.to_radians(),
            crate::viewport::aspect(&canvas),
            0.05,
            radius * 20.0,
        );
        let _ = r.update_camera(CameraMatrices {
            view,
            projection,
            position_world: eye,
            focus_distance: radius * 2.0,
            aperture: 5.6,
        });
        r.update_transforms();
        if let Err(err) = r.render(None) {
            tracing::warn!("scene demo: render error: {err}");
        }
        if f == 3 {
            let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
            let msg = js_sys::Object::new();
            set(&msg, "ready", &JsValue::TRUE);
            set(&msg, "nodes", &JsValue::from_f64(node_count as f64));
            set(&msg, "prefabs", &JsValue::from_f64(prefab_count as f64));
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
    use glam::{Mat4, Vec3};
    let arr: js_sys::Array = payload.unchecked_into();
    let dirty_addr = arr.get(0).as_f64().unwrap_or(0.0) as usize;
    let binding = SlotBinding {
        value_addr: arr.get(1).as_f64().unwrap_or(0.0) as usize,
        version_addr: arr.get(2).as_f64().unwrap_or(0.0) as usize,
        chunk: arr.get(3).as_f64().unwrap_or(0.0) as usize,
    };
    let cx = arr.get(4).as_f64().unwrap_or(0.0) as f32;
    let cy = arr.get(5).as_f64().unwrap_or(4.0) as f32;
    let cz = arr.get(6).as_f64().unwrap_or(0.0) as f32;
    let radius = arr.get(7).as_f64().unwrap_or(4.0) as f32;
    tracing::info!("scene physics worker: sweeping the runtime light");

    let tick = Rc::new(RefCell::new(0u32));
    repeat_every(16, move || {
        let t = {
            let mut tb = tick.borrow_mut();
            *tb = tb.wrapping_add(1);
            *tb
        } as f32;
        // Sweep the runtime light in a circle over the loaded scene.
        let a = t * 0.02;
        let pos = Vec3::new(cx + a.sin() * radius, cy, cz + a.cos() * radius);
        let cols = Mat4::from_translation(pos).to_cols_array();
        let bytes = unsafe { std::slice::from_raw_parts(cols.as_ptr() as *const u8, 64) };
        // SAFETY: the binding addresses the runtime light's transform slot in
        // shared memory, kept alive by the render worker for the session.
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

fn set(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}

/// A minimal **current-format** runtime [`Scene`](awsm_scene::Scene) — a floor,
/// a box, and a point light, all assigned a default material — equivalent to a
/// tiny editor export. Built in code because the repo's bundled scene fixtures
/// are a stale pre-refactor format; the value is that the player loader runs it
/// in the worker unchanged.
fn demo_scene() -> awsm_scene::Scene {
    use awsm_scene::{
        AssetEntry, AssetId, AssetSource, EditorNode, EnvironmentConfig, LightConfig, MaterialDef,
        MaterialInstance, MeshRef, MeshShadowConfig, NodeId, NodeKind, PrimitiveShape, RuntimeMesh,
        Scene, Trs,
    };

    let mut scene = Scene {
        name: "mt-scene-demo".into(),
        environment: EnvironmentConfig::default(),
        ..Default::default()
    };

    let mat = AssetId::new();
    let floor_mesh = AssetId::new();
    let box_mesh = AssetId::new();
    scene.assets.entries.insert(
        mat,
        AssetEntry::new(AssetSource::Material(MaterialDef::default())),
    );
    scene.assets.entries.insert(
        floor_mesh,
        AssetEntry::new(AssetSource::Mesh(RuntimeMesh::Primitive(
            PrimitiveShape::Box {
                dims: [20.0, 0.5, 20.0],
            },
        ))),
    );
    scene.assets.entries.insert(
        box_mesh,
        AssetEntry::new(AssetSource::Mesh(RuntimeMesh::Primitive(
            PrimitiveShape::Box {
                dims: [2.0, 2.0, 2.0],
            },
        ))),
    );

    let mesh_node = |name: &str, mesh_id: AssetId, tx: [f32; 3]| EditorNode {
        id: NodeId::new(),
        name: name.into(),
        transform: Trs {
            translation: tx,
            ..Trs::IDENTITY
        },
        kind: NodeKind::Mesh {
            mesh: MeshRef(mesh_id),
            material: Some(MaterialInstance {
                asset: mat,
                ..Default::default()
            }),
            shadow: MeshShadowConfig::default(),
        },
        locked: false,
        visible: true,
        prefab: false,
        children: vec![],
    };
    scene
        .nodes
        .push(mesh_node("floor", floor_mesh, [0.0, -2.0, 0.0]));
    scene
        .nodes
        .push(mesh_node("box", box_mesh, [0.0, 0.0, 0.0]));
    scene.nodes.push(EditorNode {
        id: NodeId::new(),
        name: "scene-light".into(),
        transform: Trs {
            translation: [4.0, 6.0, 4.0],
            ..Trs::IDENTITY
        },
        kind: NodeKind::Light(LightConfig::Point {
            color: [0.6, 0.7, 1.0],
            intensity: 60.0,
            range: 40.0,
            shadow: Default::default(),
        }),
        locked: false,
        visible: true,
        prefab: false,
        children: vec![],
    });
    scene
}
