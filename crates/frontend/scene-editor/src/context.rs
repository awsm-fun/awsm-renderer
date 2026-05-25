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
/// Pre-warmed Phase-4.3a worker pool for off-main-thread glTF parsing.
/// Built once at editor init (before APP_CONTEXT is populated) so the
/// first asset load issues `pool.dispatch::<GltfParseJob>(..)` directly
/// without paying the ~50 ms on-demand pool-build cost on top of the
/// load itself.
///
/// `None` here means the bootstrap failed (CSP that blocks blob URLs,
/// no `import.meta.url` resolution, ad-blockers nuking the worker
/// shim, the dev-only `?gltf-worker=off` opt-out, …) and
/// `asset_cache::load_and_populate` automatically routes through the
/// inline `GltfLoader::load` for the rest of the session. The
/// fallback is logged once at boot (`tracing::warn!` from
/// `maybe_build_worker_pool`); we never retry pool construction
/// in-session.
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

/// Fallible variant of [`with_camera_mut`] — returns `None` instead
/// of panicking when `create_context` hasn't completed yet. Used by
/// the canvas's event listeners (wheel / pointer), which are wired
/// up at `render_canvas` mount time *before* the async
/// `create_context` future resolves. A wheel scroll during that
/// race window (typically <100ms but not zero on slow boots) would
/// otherwise panic the wasm.
///
/// Event-handler callers should use this; explicit "I am running
/// after init" code (action handlers, render hooks) keeps using
/// [`with_camera_mut`] so a genuinely-uninitialized access stays
/// a panic rather than silently disappearing.
pub fn try_with_camera_mut<T>(f: impl FnOnce(&mut Camera) -> T) -> Option<T> {
    APP_CONTEXT.with(|ctx| {
        ctx.get().map(|ctx| {
            let mut camera = ctx.camera.lock().unwrap();
            f(&mut camera)
        })
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

/// Default-on (worker-mode gltf parsing is the editor default).
/// `?gltf-worker=off` opts out at startup — dev-only escape hatch for
/// the measurement harness's inline-baseline A/B and for testing the
/// graceful-fallback path without a CSP misconfiguration. Release
/// builds skip the URL parse entirely and always try to build the
/// pool.
#[cfg(debug_assertions)]
fn gltf_worker_enabled_from_url() -> bool {
    let Some(window) = web_sys::window() else {
        return true;
    };
    let search = window.location().search().unwrap_or_default();
    let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search) else {
        return true;
    };
    match params.get("gltf-worker").as_deref() {
        // Explicit opt-out for measurement / fallback testing.
        Some("off") => false,
        // Explicit `=on` is the legacy spelling; still honoured.
        Some(_) | None => true,
    }
}

#[cfg(not(debug_assertions))]
fn gltf_worker_enabled_from_url() -> bool {
    true
}

/// Editor default pool size. The common case is one asset load at a
/// time (user drags one glb at a time, project-open serialises the
/// asset list); 2 keeps a spare slot for the occasional parallel
/// dispatch (multi-asset import, the measurement harness). Larger
/// pools just burn boot RAM on workers that never see load —
/// `WorkerPool::with_workers(None)` would clamp to
/// `min(hardware_concurrency, 4)` but on a 16-core dev box that's
/// 4 workers permanently parked.
const GLTF_WORKER_POOL_SIZE: usize = 2;

/// Build a `WorkerPool` and register `GltfParseJob`. Returns `None` if
/// pool spawn fails — we log and degrade to the inline
/// `asset_cache::load_and_populate` path rather than refusing to boot
/// the editor. The fallback decision is sticky for the session;
/// `asset_cache` sees `worker_pool_handle().is_none()` and routes
/// every subsequent load through `GltfLoader::load`.
async fn maybe_build_worker_pool() -> Option<WorkerPool> {
    if !gltf_worker_enabled_from_url() {
        tracing::info!(
            "?gltf-worker=off — skipping WorkerPool bootstrap; asset loads will run inline"
        );
        return None;
    }
    match WorkerPool::new(WorkerPoolBootstrap::Auto, GLTF_WORKER_POOL_SIZE).await {
        Ok(pool) => {
            pool.register::<awsm_renderer_gltf::worker_job::GltfParseJob>();
            tracing::info!(
                "WorkerPool built ({GLTF_WORKER_POOL_SIZE} workers); GltfParseJob registered — \
                 asset loads will run in worker mode"
            );
            Some(pool)
        }
        Err(err) => {
            // Sticky fallback: log loudly (warn in release too — a
            // production deploy with a CSP that nukes worker bootstrap
            // wants to see this in the dev console), and proceed with
            // `None`. `asset_cache::load_and_populate` will route
            // through the inline `GltfLoader::load` path for the rest
            // of the session; we never retry construction.
            tracing::warn!(
                "WorkerPool bootstrap failed (likely CSP / blob-URL restriction / \
                 `import.meta.url` unresolved); falling back to inline glTF parse for the \
                 rest of the session: {err}"
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
    // Renderer construction (device-request, shader compile, pipeline
    // build) is the long pole — the worker pool bootstrap only needs
    // the already-loaded `WebAssembly.Module` and a few `postMessage`
    // round-trips for the `awsm-ready` handshake. Drive both
    // concurrently with `futures::future::join` so the editor's boot
    // critical path is `max(renderer, pool)` instead of
    // `renderer + pool` — matches the "in parallel with shader
    // compile" claim in `PERFORMANCE.md §5c`.
    //
    // `join` (not `try_join`) is intentional: `maybe_build_worker_pool`
    // already absorbs every failure mode into a `None` return value
    // (sticky inline fallback), so it can't error out the editor.
    // The renderer side is the only fallible path.
    let (renderer_result, worker_pool) =
        futures::future::join(create_renderer(canvas.clone()), maybe_build_worker_pool()).await;
    let renderer = renderer_result?;
    let renderer = Arc::new(xutex::AsyncMutex::new(renderer));
    let worker_pool: WorkerPoolHandle = Arc::new(worker_pool);

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

    // Pre-warmed Phase-4.3b worker pool — `Some` after a successful
    // bootstrap (the default), `None` when bootstrap failed or
    // `?gltf-worker=off` opted out. See `WorkerPoolHandle` doc.
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
        .with_phase_handler(|phase| {
            // Drive the HTML boot-loader caption (the splash shown
            // before the canvas / loading modal exist) as the
            // renderer progresses through its construction phases.
            // On a fresh Chrome profile the CompilingShaders phase
            // can last tens of seconds — Dawn lowers every WGSL
            // variant to MSL, see PERFORMANCE.md §5g — so showing a
            // phase-specific caption rather than a frozen
            // "Initializing renderer" makes the difference between
            // the user assuming the app is broken and knowing the
            // browser is doing real work that will be cached on the
            // next load.
            let msg = match phase {
                awsm_renderer::RendererLoadingPhase::Init => "Initializing renderer",
                awsm_renderer::RendererLoadingPhase::CompilingShaders => {
                    "Browser is compiling shaders (first load may take a while)"
                }
                awsm_renderer::RendererLoadingPhase::BuildingPipelines => {
                    "Building render pipelines"
                }
                awsm_renderer::RendererLoadingPhase::FinalizingScene => "Finalising renderer setup",
                awsm_renderer::RendererLoadingPhase::Ready => return,
            };
            awsm_web_shared::util::window::set_boot_loader_message(msg);
        })
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
