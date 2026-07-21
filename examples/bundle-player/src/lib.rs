//! bundle-player — a MINIMAL shipped-player-shaped page for **player visual
//! regression**: it loads ONE exported test-scene bundle
//! (`examples/test-scenes/<scene>/bundle`, served by `task test-scenes` on
//! :9084) through the real player path (`load_scene_for_player` over
//! [`HttpAssets`]) and renders it through an **authored scene camera** — the
//! camera node exported IN the bundle, not an editor/orbit rig.
//!
//! This is the piece the other tiers don't cover: the editor's goldens render
//! through editor machinery, and `examples/player-tests` checks structure
//! (counts / load transactions), never pixels. Here the pixels ARE the check:
//! a driver opens `?scene=<name>&camera=<node-name>`, waits for the `#hud`
//! `READY` line, and screenshots the 800×600 canvas against the scene's
//! committed `golden-<camera>.png`.
//!
//! Per frame the camera is re-derived from the LIVE node transform + the
//! renderer `Cameras` store — `view_from_world` + `set_camera(view, params)`,
//! exactly the consolidated camera API a real game uses — so both projections
//! (perspective AND orthographic) and even animated cameras render through
//! one code path with zero page-side camera math.
//!
//! URL params:
//! - `?scene=<name>` — the test scene (default `player-cameras`).
//! - `?camera=<node-name>` — the Camera node to render through (default: the
//!   first Camera node in authored order). Unknown name ⇒ a `FAIL` HUD line
//!   listing the scene's cameras.
//! - `?bundles=<origin>` — bundle server origin (default `http://localhost:9084`).
//!
//! The `#hud` element (positioned BELOW the 800×600 capture area) reports:
//! `bundle-player: <scene> camera=<name> (<projection>) READY frames=<n>`
//! or `bundle-player: FAIL — <why>`.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use awsm_renderer::camera::view_from_world;
use awsm_renderer::features::RendererFeatures;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};
use awsm_renderer_scene::{EditorNode, NodeKind};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

pub const CANVAS_WIDTH: u32 = 800;
pub const CANVAS_HEIGHT: u32 = 600;

/// The self-re-arming rAF closure slot (kept alive for the page's lifetime).
type RafSlot = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

#[wasm_bindgen(start)]
pub fn boot() -> Result<(), JsValue> {
    install_tracing();
    std::panic::set_hook(Box::new(|info| {
        set_hud(&format!("bundle-player: FAIL — panic: {info}"));
        web_sys::console::error_1(&format!("bundle-player panic: {info}").into());
    }));

    // Basis codec URLs (the crate hardcodes none) — same player shape as
    // examples/player-tests.
    awsm_renderer_codec_basis::configure(awsm_renderer_codec_basis::BasisWorkerConfig::player(
        "/workers/basis-worker.js".to_string(),
        "/vendor/basis/basis_transcoder.js".to_string(),
    ));

    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = run().await {
            set_hud(&format!("bundle-player: FAIL — {e:#}"));
            web_sys::console::error_1(&format!("bundle-player FAIL — {e:#}").into());
        }
    });
    Ok(())
}

async fn run() -> Result<()> {
    let scene_name = url_flag_value("scene").unwrap_or_else(|| "player-cameras".to_string());
    let origin = url_flag_value("bundles")
        .unwrap_or_else(|| "http://localhost:9084".to_string())
        .trim_end_matches('/')
        .to_string();
    set_hud(&format!("bundle-player: loading {scene_name}…"));

    // ── Renderer: PLAYER DEFAULTS (RendererFeatures::default() — reverse-Z on,
    // everything optional off), a fixed 800×600 canvas. ──
    let mut renderer = create_renderer(RendererFeatures::default()).await?;

    // ── The bundle, through the real player path. ──
    let bundle_base = format!("{origin}/{scene_name}/bundle");
    let scene = fetch_scene(&bundle_base).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
    let loaded =
        awsm_renderer_scene_loader::load_scene_for_player(&mut renderer, &scene, &assets, |_| {})
            .await
            .map_err(|e| anyhow!("load_scene_for_player: {e}"))?;
    renderer.update_transforms();

    // ── Pick the authored camera. ──
    let cameras = collect_cameras(&scene.nodes);
    if cameras.is_empty() {
        return Err(anyhow!(
            "scene `{scene_name}` has no Camera node — bundle-player renders \
             through authored cameras (add one in the editor and re-export)"
        ));
    }
    let wanted = url_flag_value("camera");
    let (cam_name, cam_id) = match &wanted {
        Some(name) => cameras
            .iter()
            .find(|(n, _)| n == name)
            .cloned()
            .ok_or_else(|| {
                let names: Vec<&str> = cameras.iter().map(|(n, _)| n.as_str()).collect();
                anyhow!("no camera named `{name}` — scene cameras: {names:?}")
            })?,
        None => cameras[0].clone(),
    };
    let handles = loaded
        .nodes
        .get(&cam_id)
        .ok_or_else(|| anyhow!("camera node `{cam_name}` was not materialized"))?;
    let cam_tk = handles.transform;
    let cam_key = handles.camera;
    let cam_cfg = handles.camera_config.clone();

    // ── Frame loop: animations → transforms → camera-from-node → render. ──
    let renderer = Rc::new(RefCell::new(renderer));
    let raf: RafSlot = Rc::new(RefCell::new(None));
    let raf_run = raf.clone();
    let mut last_ts: Option<f64> = None;
    let mut frames: u64 = 0;
    *raf.borrow_mut() = Some(Closure::new(move |ts: f64| {
        let dt_ms = last_ts.map(|p| (ts - p).max(0.0)).unwrap_or(0.0);
        last_ts = Some(ts);
        {
            let mut r = renderer.borrow_mut();
            let _ = r.update_animations(dt_ms);
            // Transforms first: the camera view comes off the node's WORLD
            // matrix, which an animated/driven camera changes every frame.
            r.update_transforms();
            let (view, params) = match camera_from_node(&r, cam_tk, cam_key, cam_cfg.as_ref()) {
                Ok(vp) => vp,
                Err(e) => {
                    set_hud(&format!("bundle-player: FAIL — camera: {e:#}"));
                    return;
                }
            };
            if let Err(e) = r.set_camera(view, params) {
                set_hud(&format!("bundle-player: FAIL — set_camera: {e}"));
                return;
            }
            if let Err(e) = r.render(None) {
                tracing::warn!("bundle-player render: {e}");
            }
        }
        frames += 1;
        // READY once the scene has had time to settle async work (pipeline
        // compiles, texture decodes). Drivers wait for this line AND their own
        // settle margin before capturing.
        if frames >= 30 {
            let projection = match cam_cfg.as_ref().map(|c| &c.projection) {
                Some(awsm_renderer_scene::CameraProjection::Orthographic { .. }) => "orthographic",
                _ => "perspective",
            };
            set_hud(&format!(
                "bundle-player: {} camera={cam_name} ({projection}) READY frames={frames}",
                url_flag_value("scene").unwrap_or_else(|| "player-cameras".to_string()),
            ));
        }
        if let Some(cb) = raf_run.borrow().as_ref() {
            let _ = awsm_renderer::web_global::request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }));
    if let Some(cb) = raf.borrow().as_ref() {
        let _ = awsm_renderer::web_global::request_animation_frame(cb.as_ref().unchecked_ref());
    }
    // The loop owns the renderer for the page's lifetime.
    std::mem::forget(raf);
    Ok(())
}

/// `(view, params)` for a scene camera node, re-derived from the LIVE world
/// transform + the renderer `Cameras` store (the animatable source — the store
/// slot mirrors the authored config and is what an `AnimationTarget::Camera`
/// channel drives), falling back to the bundled config. This is the whole
/// camera "rig" a player needs under the consolidated API.
fn camera_from_node(
    renderer: &AwsmRenderer,
    tk: awsm_renderer::transforms::TransformKey,
    camera_key: Option<awsm_renderer::camera::CameraKey>,
    cfg: Option<&awsm_renderer_scene::CameraConfig>,
) -> Result<(glam::Mat4, awsm_renderer::camera::CameraParams)> {
    let world = *renderer
        .transforms
        .get_world(tk)
        .map_err(|e| anyhow!("camera world transform: {e}"))?;
    let view = view_from_world(world);
    let params = camera_key
        .and_then(|key| renderer.cameras.get(key))
        .copied()
        .or_else(|| cfg.map(awsm_renderer_scene_loader::camera::camera_params_from_config))
        .ok_or_else(|| anyhow!("camera node has neither a store slot nor a config"))?;
    Ok((view, params))
}

/// `(name, id)` of every Camera node, authored (DFS) order.
fn collect_cameras(nodes: &[EditorNode]) -> Vec<(String, awsm_renderer_scene::NodeId)> {
    let mut out = Vec::new();
    fn walk(nodes: &[EditorNode], out: &mut Vec<(String, awsm_renderer_scene::NodeId)>) {
        for n in nodes {
            if matches!(n.kind, NodeKind::Camera(_)) {
                out.push((n.name.clone(), n.id));
            }
            walk(&n.children, out);
        }
    }
    walk(nodes, &mut out);
    out
}

/// Fresh renderer on a fixed 800×600 canvas at the page's top-left (the golden
/// capture area).
async fn create_renderer(features: RendererFeatures) -> Result<AwsmRenderer> {
    let window = web_sys::window().ok_or_else(|| anyhow!("no window"))?;
    let document = window.document().ok_or_else(|| anyhow!("no document"))?;
    let canvas: web_sys::HtmlCanvasElement = document
        .create_element("canvas")
        .map_err(|e| anyhow!("create canvas: {e:?}"))?
        .dyn_into()
        .map_err(|_| anyhow!("canvas cast"))?;
    canvas.set_id("render-canvas");
    canvas.set_width(CANVAS_WIDTH);
    canvas.set_height(CANVAS_HEIGHT);
    document
        .body()
        .ok_or_else(|| anyhow!("no body"))?
        .append_child(&canvas)
        .map_err(|e| anyhow!("append canvas: {e:?}"))?;

    let gpu = window.navigator().gpu();
    let gpu_builder = AwsmRendererWebGpuBuilder::new(gpu, canvas)
        .with_device_request_limits(DeviceRequestLimits::max_all());
    awsm_renderer::AwsmRendererBuilder::new(gpu_builder)
        .with_features(features)
        .build()
        .await
        .map_err(|e| anyhow!("renderer build: {e}"))
}

/// Fetch + parse `<bundle_base>/scene.toml`.
async fn fetch_scene(bundle_base: &str) -> Result<awsm_renderer_scene::Scene> {
    let url = format!("{bundle_base}/scene.toml");
    let text = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("fetch {url}: {e}"))?
        .text()
        .await
        .map_err(|e| anyhow!("read {url}: {e}"))?;
    awsm_renderer_scene::project_dir::scene_from_toml(&text)
        .map_err(|e| anyhow!("parse {url}: {e}"))
}

fn set_hud(text: &str) {
    if let Some(hud) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("hud"))
    {
        hud.set_text_content(Some(text));
    }
}

/// The `…` of `?<key>=…` in the page URL, if present.
fn url_flag_value(key: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let prefix = format!("{key}=");
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|p| p.strip_prefix(&prefix).map(|v| v.to_string()))
}

fn install_tracing() {
    use tracing_subscriber::prelude::*;
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(tracing_web::MakeWebConsoleWriter::new())
        .with_target(false);
    let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
}
