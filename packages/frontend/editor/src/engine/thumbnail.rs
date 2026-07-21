//! Material thumbnail renderer. A small, hidden, offscreen `AwsmRenderer` shades a
//! sphere with each **built-in** library material's variant and captures it to a
//! PNG data URL (`canvas.toDataURL`), shown on the Content-Browser / Material-
//! library cards. Reuses the preview renderer's sphere + lighting + renderer build.
//!
//! Built-in only: their variant compiles synchronously (`material_to_renderer`),
//! so the frame captured right after `render()` is correct. Dynamic WGSL materials
//! compile asynchronously and already have the Studio's live preview, so they keep
//! their flat debug-color swatch here. Lazy: the renderer builds on first request;
//! one material is rendered per animation frame so a big library never stalls a frame.

#![allow(clippy::arc_with_non_send_sync)]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use awsm_renderer::bounds::Aabb;
use awsm_renderer::lights::Light;
use awsm_renderer::meshes::MeshKey;
use awsm_renderer::transforms::Transform;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_editor_protocol::MaterialDef;
use awsm_renderer_web_shared::prelude::Mutable;
use awsm_renderer_web_shared::util::free_camera::FreeCamera as Camera;
use gloo_render::AnimationFrame;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use super::bridge::material as bmat;
use super::preview;
use crate::controller::CustomMaterial;
use crate::engine::scene::AssetId;

/// Thumbnail resolution (square). Small → fast capture + compact data URLs.
const SIZE: u32 = 128;

thread_local! {
    /// id → PNG data URL. Cards observe this; a new entry appears when its render lands.
    static THUMBS: Mutable<HashMap<AssetId, String>> = Mutable::new(HashMap::new());
    static GEN: RefCell<Option<Arc<Gen>>> = const { RefCell::new(None) };
    static BUILDING: Cell<bool> = const { Cell::new(false) };
    /// Materials waiting to be rendered (decoupled from `Gen` so requests made
    /// before the renderer finishes building aren't lost).
    static PENDING: RefCell<VecDeque<Arc<CustomMaterial>>> = const { RefCell::new(VecDeque::new()) };
    /// Ids already queued/rendered — dedup so a re-rendered card list doesn't re-enqueue.
    static SEEN: RefCell<Option<HashSet<AssetId>>> = const { RefCell::new(None) };
}

/// Reactive id → data-URL map for the cards to observe.
pub fn thumbnails() -> Mutable<HashMap<AssetId, String>> {
    THUMBS.with(|t| t.clone())
}

/// Queue a thumbnail render for `mat` (built-in only; no-op once generated —
/// call [`invalidate`] to force a refresh after a variant edit).
pub fn request(mat: Arc<CustomMaterial>) {
    if !mat.is_builtin() {
        return;
    }
    let id = mat.id;
    let fresh = SEEN.with(|s| s.borrow_mut().get_or_insert_with(HashSet::new).insert(id));
    if !fresh {
        return;
    }
    PENDING.with(|p| p.borrow_mut().push_back(mat));
    ensure_gen();
}

/// Drop a material's cached thumbnail so the next [`request`] re-renders it
/// (called when a built-in's variant settings change).
pub fn invalidate(id: AssetId) {
    SEEN.with(|s| {
        if let Some(set) = s.borrow_mut().as_mut() {
            set.remove(&id);
        }
    });
    THUMBS.with(|t| {
        t.lock_mut().remove(&id);
    });
}

struct Gen {
    renderer: Arc<xutex::AsyncMutex<AwsmRenderer>>,
    camera: Arc<Mutex<Camera>>,
    canvas: web_sys::HtmlCanvasElement,
    mesh: MeshKey,
    /// An async `set_material` is in flight (skip starting another).
    busy: Cell<bool>,
    /// Frames left to render the current material before capturing it. `-1` = idle.
    /// A WebGPU canvas only reliably reads back via `toDataURL` while it's being
    /// actively presented, so we render a few live frames before the capture.
    capture: Cell<i32>,
    capturing_id: Cell<Option<AssetId>>,
    raf: RefCell<Option<AnimationFrame>>,
}

fn ensure_gen() {
    if GEN.with(|g| g.borrow().is_some()) || BUILDING.with(|b| b.get()) {
        return;
    }
    BUILDING.with(|b| b.set(true));
    let canvas = make_hidden_canvas();
    spawn_local(async move {
        match build(canvas).await {
            Ok(gen) => {
                GEN.with(|g| *g.borrow_mut() = Some(gen.clone()));
                start_raf(gen);
            }
            Err(e) => tracing::warn!("thumbnail renderer build failed: {e}"),
        }
        BUILDING.with(|b| b.set(false));
    });
}

fn make_hidden_canvas() -> web_sys::HtmlCanvasElement {
    let doc = web_sys::window().unwrap().document().unwrap();
    let canvas: web_sys::HtmlCanvasElement =
        doc.create_element("canvas").unwrap().dyn_into().unwrap();
    canvas.set_width(SIZE);
    canvas.set_height(SIZE);
    let style = canvas.style();
    // Off-screen but laid-out (a `display:none` canvas has no drawing buffer).
    let _ = style.set_property("position", "fixed");
    let _ = style.set_property("left", "-10000px");
    let _ = style.set_property("top", "0");
    let _ = style.set_property("width", "128px");
    let _ = style.set_property("height", "128px");
    let _ = style.set_property("pointer-events", "none");
    if let Some(body) = doc.body() {
        let _ = body.append_child(&canvas);
    }
    canvas
}

async fn build(canvas: web_sys::HtmlCanvasElement) -> Result<Arc<Gen>, String> {
    let renderer = preview::build_renderer(canvas.clone()).await?;
    let renderer = Arc::new(xutex::AsyncMutex::new(renderer));
    // Frame tightly on the 0.85-radius preview sphere (not the 80 m default cube).
    // Depth convention (003): thumbnails follow the main renderer's flag —
    // the camera only uses it for its clip-plane policy.
    let camera = Arc::new(Mutex::new(Camera::new_aabb(
        Aabb::new_cube(1.8, 1.8),
        1.4,
        awsm_renderer::depth_convention::DepthConvention {
            reverse_z: crate::engine::context::reverse_z_flag(),
        },
    )));

    let mesh = {
        let mut r = renderer.lock().await;
        let _ = r.insert_light(
            Light::Directional {
                color: [1.0, 1.0, 1.0],
                intensity: 3.0,
                direction: [-0.4, -0.75, -0.55],
            },
            None,
        );
        let tk = r.transforms.insert(Transform::IDENTITY, None);
        let key = preview::insert_material_into(
            &mut r,
            bmat::material_to_renderer(&MaterialDef::default()),
        );
        let mk = r
            .add_raw_mesh(preview::preview_sphere(), tk, key)
            .map_err(|e| format!("{e}"))?;
        // Commit the staged preview content (this also flips the thumbnail
        // renderer's gate open so `render()` draws the scene, not a clear).
        if let Err(e) = r.commit_load(|_| {}).await {
            tracing::warn!("thumbnail commit_load: {e}");
        }
        mk
    };

    Ok(Arc::new(Gen {
        renderer,
        camera,
        canvas,
        mesh,
        busy: Cell::new(false),
        capture: Cell::new(-1),
        capturing_id: Cell::new(None),
        raf: RefCell::new(None),
    }))
}

/// Number of live frames to render a material before grabbing its thumbnail.
const CAPTURE_DELAY_FRAMES: i32 = 3;

fn start_raf(gen: Arc<Gen>) {
    let again = gen.clone();
    let raf = gloo_render::request_animation_frame(move |_| {
        tick(&again);
        start_raf(again.clone());
    });
    *gen.raf.borrow_mut() = Some(raf);
}

fn tick(gen: &Arc<Gen>) {
    // Keep the canvas presenting a fresh frame every tick (so `toDataURL` reads back).
    if let Some(mut r) = gen.renderer.try_lock() {
        let (view, params) = {
            let c = gen.camera.lock().unwrap();
            (c.view(), c.params())
        };
        let _ = r.set_camera(view, params);
        r.update_transforms();
        let _ = r.render(None);
    }

    let c = gen.capture.get();
    if c > 0 {
        gen.capture.set(c - 1);
        if c - 1 == 0 {
            // The current material has rendered for a few live frames — capture it.
            if let (Ok(url), Some(id)) = (
                gen.canvas.to_data_url_with_type("image/png"),
                gen.capturing_id.get(),
            ) {
                THUMBS.with(|t| {
                    t.lock_mut().insert(id, url);
                });
            }
            gen.capturing_id.set(None);
            gen.capture.set(-1);
        }
        return;
    }

    // Idle → start the next queued material (its set is async; the frame countdown
    // begins once the material is on the sphere).
    if !gen.busy.get() {
        if let Some(mat) = PENDING.with(|p| p.borrow_mut().pop_front()) {
            gen.busy.set(true);
            let g = gen.clone();
            spawn_local(async move {
                if let Err(e) = set_material(&g, &mat).await {
                    tracing::warn!("thumbnail set_material failed: {e}");
                } else {
                    g.capturing_id.set(Some(mat.id));
                    g.capture.set(CAPTURE_DELAY_FRAMES);
                }
                g.busy.set(false);
            });
        }
    }
}

async fn set_material(gen: &Arc<Gen>, mat: &CustomMaterial) -> Result<(), String> {
    let Some(def) = mat.builtin.get_cloned() else {
        return Ok(());
    };
    let mut r = gen.renderer.lock().await;
    let key = preview::insert_material_into(&mut r, bmat::material_to_renderer(&def));
    let _ = r.set_mesh_material(gen.mesh, key);
    if let Err(e) = r.commit_load(|_| {}).await {
        tracing::warn!("thumbnail commit_load: {e}");
    }
    Ok(())
}
