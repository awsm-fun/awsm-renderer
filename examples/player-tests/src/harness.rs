//! Shared plumbing: fresh renderer per bundle, bundle fetch/parse, camera
//! framing, and the requestAnimationFrame drive loop.

use anyhow::{anyhow, Context, Result};
use awsm_renderer::camera::CameraMatrices;
use awsm_renderer::features::RendererFeatures;
use awsm_renderer::AwsmRenderer;
use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};
use glam::{Mat4, Vec3};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

pub const CANVAS_WIDTH: u32 = 800;
pub const CANVAS_HEIGHT: u32 = 600;

/// Build a **fresh** renderer (own canvas + own device) — every bundle load in
/// this harness is cold and isolated, the cleanest per-scene pattern. The
/// previous scene's canvas is removed; call [`destroy_renderer`] on the old
/// renderer first so its device is released explicitly.
pub async fn create_renderer(
    features: RendererFeatures,
) -> Result<(AwsmRenderer, web_sys::HtmlCanvasElement)> {
    let window = web_sys::window().ok_or_else(|| anyhow!("no window"))?;
    let document = window.document().ok_or_else(|| anyhow!("no document"))?;
    if let Some(prev) = document.get_element_by_id("render-canvas") {
        prev.remove();
    }
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
    let gpu_builder = AwsmRendererWebGpuBuilder::new(gpu, canvas.clone())
        .with_device_request_limits(DeviceRequestLimits::max_all());
    // Force the GPU-driven culling path (like the editor) so the harness
    // exercises the pipeline players get on big scenes regardless of size.
    let policy = awsm_renderer::optimization_policy::RendererOptimizationPolicy {
        gpu_culling: awsm_renderer::optimization_policy::OptimizationMode::Force,
        ..Default::default()
    };
    let renderer = awsm_renderer::AwsmRendererBuilder::new(gpu_builder)
        .with_features(features)
        .with_optimization_policy(policy)
        .build()
        .await
        .map_err(|e| anyhow!("renderer build: {e}"))?;
    Ok((renderer, canvas))
}

/// Release the renderer's GPU device explicitly (dropping alone leaves the
/// device to GC; the harness creates ~10 renderers per run).
pub fn destroy_renderer(renderer: AwsmRenderer) {
    renderer.gpu.device.destroy();
    drop(renderer);
}

/// Fetch + parse a bundle's `scene.toml` from
/// `<origin>/<scene>/bundle/scene.toml`.
pub async fn fetch_scene(bundle_base: &str) -> Result<awsm_renderer_scene::Scene> {
    let url = format!("{bundle_base}/scene.toml");
    let text = gloo_net::http::Request::get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("fetch {url}: {e}"))?
        .text()
        .await
        .map_err(|e| anyhow!("read {url}: {e}"))?;
    awsm_renderer_scene::project_dir::scene_from_toml(&text).with_context(|| format!("parse {url}"))
}

/// Total authored node count (roots + descendants) — the rough expectation the
/// counts check compares materialized nodes against.
pub fn count_authored_nodes(nodes: &[awsm_renderer_scene::EditorNode]) -> usize {
    nodes
        .iter()
        .map(|n| 1 + count_authored_nodes(&n.children))
        .sum()
}

/// World bounds of everything the load materialized (after
/// `update_transforms`), with a fallback for empty/unbounded scenes.
pub fn scene_bounds(renderer: &AwsmRenderer) -> (Vec3, f32) {
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for node in renderer.scene_spatial.iter_all() {
        lo = lo.min(node.aabb.min);
        hi = hi.max(node.aabb.max);
    }
    if !lo.is_finite() || !hi.is_finite() {
        lo = Vec3::splat(-4.0);
        hi = Vec3::splat(4.0);
    }
    let center = (lo + hi) * 0.5;
    let radius = ((hi - lo).length() * 0.5).max(1.0);
    (center, radius)
}

/// Point the camera at `center` from `eye`. Examples stay forward-Z (the
/// renderer features default; docs/plans/003) so `reverse_z` is `false` here
/// and in the features structs.
pub fn set_camera(renderer: &mut AwsmRenderer, eye: Vec3, center: Vec3, radius: f32) -> Result<()> {
    let aspect = CANVAS_WIDTH as f32 / CANVAS_HEIGHT as f32;
    let near = (radius * 0.001).max(0.01);
    let far = (radius * 200.0).max(100.0);
    let view = Mat4::look_at_rh(eye, center, Vec3::Y);
    // One source for the projection AND the reverse_z flag below, so
    // the two cannot drift — the renderer owns the convention.
    let convention = renderer.features.depth();
    let projection = convention.perspective(45.0_f32.to_radians(), aspect, near, far);
    renderer
        .update_camera(CameraMatrices {
            view,
            projection,
            position_world: eye,
            focus_distance: (eye - center).length().max(0.1),
            aperture: 5.6,
            reverse_z: convention.reverse_z,
            near,
            far,
        })
        .map_err(|e| anyhow!("update_camera: {e}"))
}

/// Await the next `requestAnimationFrame`, returning its DOMHighResTimeStamp.
pub async fn next_frame() -> f64 {
    let (tx, rx) = futures::channel::oneshot::channel::<f64>();
    let cb = Closure::once_into_js(move |t: f64| {
        let _ = tx.send(t);
    });
    let _ = awsm_renderer::web_global::request_animation_frame(cb.unchecked_ref());
    rx.await.unwrap_or(0.0)
}

/// Drive `frames` rAF-aligned frames: per frame set the camera at `eye(i)`,
/// update transforms, render. Returns the rAF timestamps (frames + 1 stamps ⇒
/// `frames` deltas).
pub async fn run_frames(
    renderer: &mut AwsmRenderer,
    center: Vec3,
    radius: f32,
    frames: usize,
    mut eye: impl FnMut(usize) -> Vec3,
) -> Result<Vec<f64>> {
    let mut stamps = Vec::with_capacity(frames + 1);
    stamps.push(next_frame().await);
    for i in 0..frames {
        set_camera(renderer, eye(i), center, radius)?;
        renderer.update_transforms();
        renderer.render(None).map_err(|e| anyhow!("render: {e}"))?;
        stamps.push(next_frame().await);
    }
    Ok(stamps)
}

/// Idle-soak mode (`?soak=<scene>`): load ONE bundle through the player path and
/// idle-render it forever with a STATIC camera — the player-side analogue of the
/// editor idle soak (`tools/soak/soak.mjs`). Used to answer whether the SHARED
/// render core (present-view / submit / occlusion-cull readbacks — `gpu_culling`
/// is forced in [`create_renderer`]) leaks native VM regions on its own, with
/// none of the editor's overlay / gizmo / picker / HUD / MCP machinery present.
/// Never returns.
pub async fn run_soak(origin: &str, scene_name: &str, features: RendererFeatures) -> Result<()> {
    let bundle_base = format!("{origin}/{scene_name}/bundle");
    let scene = fetch_scene(&bundle_base).await?;
    let (mut renderer, _canvas) = create_renderer(features).await?;
    let assets = awsm_renderer_scene_loader::assets::HttpAssets::new(bundle_base);
    awsm_renderer_scene_loader::load_scene_for_player(&mut renderer, &scene, &assets, |_| {})
        .await
        .map_err(|e| anyhow!("load_scene_for_player: {e}"))?;
    renderer.update_transforms();
    let (center, radius) = scene_bounds(&renderer);
    // Fixed idle camera (no orbit) — matches the editor idle soak so the only
    // per-frame work is the render loop itself.
    let eye = orbit_eye(center, radius, 2.2, 0.8);
    set_camera(&mut renderer, eye, center, radius)?;
    tracing::info!("player-soak: idle-rendering {scene_name} forever (shared-core leak probe)");
    loop {
        next_frame().await;
        renderer.update_transforms();
        renderer.render(None).map_err(|e| anyhow!("render: {e}"))?;
    }
}

/// A fixed 3/4-view eye at `distance_factor × radius` from the bounds center.
pub fn orbit_eye(center: Vec3, radius: f32, distance_factor: f32, yaw: f32) -> Vec3 {
    center
        + Vec3::new(
            yaw.sin() * radius * distance_factor,
            radius * distance_factor * 0.5,
            yaw.cos() * radius * distance_factor,
        )
}

/// `performance.now()` in ms.
pub fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

/// True when `?<key>` (or `?<key>=…`) is present in the page URL (mirrors the
/// editor's flag reader so `?stream` / `?streambudget=N` behave identically).
pub fn url_has_flag(key: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .map(|search| {
            let q = search.trim_start_matches('?');
            q.split('&')
                .any(|p| p == key || p.starts_with(&format!("{key}=")))
        })
        .unwrap_or(false)
}

/// The `…` of `?<key>=…` in the page URL, if present.
pub fn url_flag_value(key: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let prefix = format!("{key}=");
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|p| p.strip_prefix(&prefix).map(|v| v.to_string()))
}
