#![allow(
    dead_code,
    clippy::arc_with_non_send_sync,
    clippy::manual_map,
    clippy::missing_const_for_thread_local
)]

use std::sync::{Arc, OnceLock};

use awsm_renderer::{
    debug::AwsmRendererLogging,
    features::RendererFeatures,
    render::RenderHooks,
    workers::{WorkerPool, WorkerPoolBootstrap},
    AwsmRenderer, AwsmRendererBuilder,
};
use awsm_renderer_core::{
    command::color::Color,
    configuration::{CanvasAlphaMode, CanvasConfiguration, CanvasToneMappingMode},
    renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits},
};
use awsm_web::dom::resize::ResizeObserver;
use awsm_web_shared::prelude::AsyncLoader;
use dominator::clone;
use gloo_render::AnimationFrame;

use crate::error::EditorResult;
use awsm_web_shared::util::free_camera::FreeCamera as Camera;

pub type RendererHandle = Arc<xutex::AsyncMutex<AwsmRenderer>>;
pub type CameraHandle = Arc<std::sync::Mutex<Camera>>;
pub type RenderHooksHandle = Arc<std::sync::RwLock<Option<RenderHooks>>>;
/// Optional Phase-4.3a worker pool used for off-main-thread glTF
/// parsing when the dev-only `?gltf-worker=on` URL knob is set. The
/// pool is built once at editor init; `None` keeps the inline
/// `GltfLoader::load` path (the default).
pub type WorkerPoolHandle = Arc<Option<WorkerPool>>;

// we expose these public functions, and internally hold static locks
pub fn with_canvas<T>(f: impl FnOnce(&web_sys::HtmlCanvasElement) -> T) -> T {
    APP_CONTEXT.with(|ctx| {
        if let Some(ctx) = ctx.get() {
            f(&ctx.canvas)
        } else {
            panic!("AppContext not initialized when trying to access canvas");
        }
    })
}

pub async fn with_renderer_mut<T>(f: impl FnOnce(&mut AwsmRenderer) -> T) -> T {
    let handle = APP_CONTEXT.with(|ctx| {
        if let Some(ctx) = ctx.get() {
            Some(ctx.renderer.clone())
        } else {
            None
        }
    });

    match handle {
        Some(handle) => {
            let mut renderer = handle.lock().await;
            f(&mut renderer)
        }
        None => {
            panic!("AppContext not initialized when trying to access renderer");
        }
    }
}

pub async fn with_renderer<T>(f: impl FnOnce(&AwsmRenderer) -> T) -> T {
    let handle = APP_CONTEXT.with(|ctx| {
        if let Some(ctx) = ctx.get() {
            Some(ctx.renderer.clone())
        } else {
            None
        }
    });

    match handle {
        Some(handle) => {
            let renderer = handle.lock().await;
            f(&renderer)
        }
        None => {
            panic!("AppContext not initialized when trying to access renderer");
        }
    }
}

pub fn with_camera_mut<T>(f: impl FnOnce(&mut Camera) -> T) -> T {
    APP_CONTEXT.with(|ctx| {
        if let Some(ctx) = ctx.get() {
            let mut camera = ctx.camera.lock().unwrap();
            f(&mut camera)
        } else {
            panic!("AppContext not initialized when trying to access camera");
        }
    })
}

pub fn with_camera<T>(f: impl FnOnce(&Camera) -> T) -> T {
    APP_CONTEXT.with(|ctx| {
        if let Some(ctx) = ctx.get() {
            let camera = ctx.camera.lock().unwrap();
            f(&camera)
        } else {
            panic!("AppContext not initialized when trying to access camera");
        }
    })
}

pub fn set_raf(raf: AnimationFrame) {
    APP_CONTEXT.with(|ctx| {
        if let Some(ctx) = ctx.get() {
            *ctx.raf.lock().unwrap() = Some(raf);
        } else {
            panic!("AppContext not initialized when trying to set tick");
        }
    });
}

/// Raw handle for callers that need to hold the renderer lock across
/// multiple awaits (e.g. the asset cache's populate-gltf flow).
pub fn renderer_handle() -> RendererHandle {
    APP_CONTEXT.with(|ctx| {
        ctx.get()
            .expect("AppContext not initialized")
            .renderer
            .clone()
    })
}

pub fn camera_handle() -> CameraHandle {
    APP_CONTEXT.with(|ctx| {
        ctx.get()
            .expect("AppContext not initialized")
            .camera
            .clone()
    })
}

pub fn render_hooks_handle() -> RenderHooksHandle {
    APP_CONTEXT.with(|ctx| {
        ctx.get()
            .expect("AppContext not initialized")
            .render_hooks
            .clone()
    })
}

pub fn set_render_hooks(hooks: RenderHooks) {
    APP_CONTEXT.with(|ctx| {
        if let Some(ctx) = ctx.get() {
            *ctx.render_hooks.write().unwrap() = Some(hooks);
        } else {
            panic!("AppContext not initialized when trying to access render hooks");
        }
    });
}

/// Read `?gltf-worker=on` from the URL. Dev-only knob; release builds
/// always return `false` so production never spawns the pool.
#[cfg(debug_assertions)]
fn gltf_worker_enabled_from_url() -> bool {
    web_sys::window()
        .and_then(|w| {
            let search = w.location().search().unwrap_or_default();
            web_sys::UrlSearchParams::new_with_str(&search).ok()
        })
        .and_then(|p| p.get("gltf-worker"))
        .map(|v| v == "on")
        .unwrap_or(false)
}

#[cfg(not(debug_assertions))]
fn gltf_worker_enabled_from_url() -> bool {
    false
}

/// Build a `WorkerPool` and register `GltfParseJob`. Returns `None` if
/// pool spawn fails (we log and degrade to the inline path rather
/// than refusing to boot the editor).
async fn maybe_build_worker_pool() -> Option<WorkerPool> {
    if !gltf_worker_enabled_from_url() {
        return None;
    }
    match WorkerPool::new(WorkerPoolBootstrap::Auto, 2).await {
        Ok(pool) => {
            pool.register::<awsm_renderer_gltf::worker_job::GltfParseJob>();
            tracing::info!("?gltf-worker=on — WorkerPool built (2 workers); GltfParseJob registered");
            Some(pool)
        }
        Err(err) => {
            tracing::warn!(
                "?gltf-worker=on but WorkerPool construction failed; falling back to inline path: {err}"
            );
            None
        }
    }
}

pub fn worker_pool_handle() -> WorkerPoolHandle {
    APP_CONTEXT.with(|ctx| {
        ctx.get()
            .expect("AppContext not initialized")
            .worker_pool
            .clone()
    })
}

// Called once at init
pub async fn create_context(canvas: web_sys::HtmlCanvasElement) -> EditorResult<()> {
    let renderer = create_renderer(canvas.clone()).await?;
    let renderer = Arc::new(xutex::AsyncMutex::new(renderer));
    let worker_pool: WorkerPoolHandle = Arc::new(maybe_build_worker_pool().await);

    let camera = {
        let mut cam = Camera::new_default_cube(16.0 / 9.0);
        cam.set_aperture(crate::config::CONFIG.camera_aperture);
        cam.set_focus_distance(crate::config::CONFIG.camera_focus_distance);
        Arc::new(std::sync::Mutex::new(cam))
    };

    let render_hooks = Arc::new(std::sync::RwLock::new(None));

    let resize_observer = create_resize_observer(
        canvas.clone(),
        renderer.clone(),
        camera.clone(),
        render_hooks.clone(),
    );

    let ctx = AppContext {
        raf: Arc::new(std::sync::Mutex::new(None)),
        canvas,
        renderer,
        camera,
        render_hooks,
        resize_observer: Arc::new(resize_observer),
        worker_pool,
        _drop_tracker: Arc::new(AppContextDropTracker),
    };

    let _ = APP_CONTEXT.with(|x| x.set(ctx));

    Ok(())
}

// from here on it's all private
thread_local! {
    static APP_CONTEXT: OnceLock<AppContext> = OnceLock::new();
}

#[derive(Clone)]
struct AppContext {
    canvas: web_sys::HtmlCanvasElement,
    // needs to hold a lock over await points
    renderer: RendererHandle,
    // does _not_ need to hold a lock over await points, so we can use a regular mutex
    camera: CameraHandle,

    // just holding so we don't drop it
    resize_observer: Arc<ResizeObserver>,

    // for hooking into our render passes
    render_hooks: RenderHooksHandle,

    // we need to hold the RAF closure here to keep it alive
    raf: Arc<std::sync::Mutex<Option<AnimationFrame>>>,

    // Optional Phase-4.3b worker pool — `Some` when `?gltf-worker=on`
    // is set, otherwise `None` and asset_cache uses the inline path.
    worker_pool: WorkerPoolHandle,

    // just for debugging
    _drop_tracker: Arc<AppContextDropTracker>,
}

/// Reads the renderer-feature gate from the current URL's query
/// string. Defaults to `gpu_culling = true, decals = true` so the
/// editor's default boot is unchanged; `?features=off` disables
/// both gates so the measurement harness can A/B them.
///
/// `#[cfg(debug_assertions)]`-gated — release builds skip the URL
/// parse entirely and always boot with both features on. Dev-only
/// escape hatch for the measurement harness; production users have
/// no way to flip features at runtime.
#[cfg(debug_assertions)]
fn parse_features_from_url() -> RendererFeatures {
    use awsm_renderer::features::FeatureToggle;
    // `coverage_lod` stays off — both its consumers (skin-skip,
    // cheap-material LOD) are parked, so engaging the producer in the
    // editor would just be measurement noise. Flip on per-build when
    // you wire up a consumer.
    //
    // `indirect_first_instance` defaults to Auto (capability-detect).
    // The dev-only `?ifi=off` / `?ifi=on` query knob below forces the
    // portable / optimized path respectively so the test harness can
    // exercise both code paths on a single machine.
    let mut on = RendererFeatures {
        gpu_culling: true,
        decals: true,
        coverage_lod: false,
        indirect_first_instance: FeatureToggle::Auto,
    };
    let Some(window) = web_sys::window() else {
        return on;
    };
    let search = window.location().search().unwrap_or_default();
    let params = match web_sys::UrlSearchParams::new_with_str(&search) {
        Ok(p) => p,
        Err(_) => return on,
    };
    match params.get("ifi").as_deref() {
        Some("on") => on.indirect_first_instance = FeatureToggle::On,
        Some("off") => on.indirect_first_instance = FeatureToggle::Off,
        Some("auto") | None => {} // already Auto
        Some(other) => {
            tracing::warn!("unrecognized ?ifi value {other:?} — falling back to Auto");
        }
    }
    match params.get("features").as_deref() {
        Some("off") => RendererFeatures::default(),
        _ => on,
    }
}

#[cfg(not(debug_assertions))]
fn parse_features_from_url() -> RendererFeatures {
    RendererFeatures {
        gpu_culling: true,
        decals: true,
        coverage_lod: false,
        indirect_first_instance: awsm_renderer::features::FeatureToggle::Auto,
    }
}

async fn create_renderer(canvas: web_sys::HtmlCanvasElement) -> EditorResult<AwsmRenderer> {
    let gpu = web_sys::window().unwrap().navigator().gpu();
    let gpu_builder = AwsmRendererWebGpuBuilder::new(gpu, canvas)
        .with_configuration(
            CanvasConfiguration::default()
                .with_alpha_mode(CanvasAlphaMode::Opaque)
                .with_tone_mapping(CanvasToneMappingMode::Standard),
        )
        .with_device_request_limits(DeviceRequestLimits::max_all());

    // Editor opts into the full GPU-driven pipeline + decals.
    // Library consumers / runtime games choose their own feature set
    // via `with_features`.
    //
    // Dev-only escape hatch: a `?features=off` query param disables
    // both gates so the measurement harness can A/B the always-on
    // overhead against the default editor build. The flag
    // is read once at construction; toggling it requires a page
    // reload (the renderer's gated fields are populated at build
    // time). Falls back to "both on" for any other value (including
    // missing).
    let features = parse_features_from_url();
    // Editor wants the GPU-driven path engaged regardless of scene
    // size so it's actually exercised during authoring (Auto would
    // park it off below ~500 opaque meshes, hiding regressions until
    // someone loads a heavy scene). The runtime default of `Auto` is
    // right for shipping games; editor overrides with `Force`. The
    // `?features=off` escape hatch already drops the capability via
    // `with_features`, which makes the policy decision moot.
    let policy = awsm_renderer::optimization_policy::RendererOptimizationPolicy {
        gpu_culling: awsm_renderer::optimization_policy::OptimizationMode::Force,
        ..Default::default()
    };
    let renderer = AwsmRendererBuilder::new(gpu_builder)
        .with_logging(AwsmRendererLogging {
            render_timings: cfg!(debug_assertions),
        })
        .with_clear_color(Color::MID_GREY)
        .with_features(features)
        .with_optimization_policy(policy)
        .build()
        .await?;

    Ok(renderer)
}

fn create_resize_observer(
    canvas: web_sys::HtmlCanvasElement,
    renderer: RendererHandle,
    camera: CameraHandle,
    render_hooks: RenderHooksHandle,
) -> ResizeObserver {
    let loader = AsyncLoader::new();

    let resize_observer = ResizeObserver::new(
        move |entries| {
            tracing::info!("canvas resized");
            loader.load(clone!(camera, render_hooks, renderer => async move {
                if let Some(entry) = entries.first() {
                    let width = entry.content_box_sizes[0].inline_size;
                    let height = entry.content_box_sizes[0].block_size;

                    if width == 0 || height == 0 {
                        return;
                    }

                    let mut renderer = renderer.lock().await;

                    renderer.gpu.canvas().set_width(width);
                    renderer.gpu.canvas().set_height(height);

                    let camera_aspect = width as f32 / height as f32;


                    renderer.gpu.sync_canvas_buffer_with_css();

                    let camera_matrices = {
                        let mut camera = camera.lock().unwrap();
                        camera.set_aspect(camera_aspect);
                        camera.matrices()
                    };

                    if let Err(err) = renderer.update_camera(camera_matrices) {
                        tracing::error!("Error updating camera on resize: {:?}", err);
                    }

                    let hooks = render_hooks.read().unwrap();
                    if let Err(err) = renderer.render(hooks.as_ref()) {
                        tracing::error!("Error rendering on resize: {:?}", err);
                    }
                }
            }));
        },
        None,
    );

    resize_observer.observe(&canvas);

    resize_observer
}

struct AppContextDropTracker;

impl Drop for AppContextDropTracker {
    fn drop(&mut self) {
        tracing::error!("AppContext dropped!");
    }
}
