//! WebGPU renderer context: the renderer handle, editor camera, resize observer,
//! worker pool, and the boot (`create_context`). Adapted from the archived
//! editor (UI-agnostic engine plumbing); the editor/project *state* lives in the
//! `EditorController`, this module only owns the renderer-side handles.

#![allow(
    dead_code,
    clippy::arc_with_non_send_sync,
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
use awsm_web_shared::util::free_camera::FreeCamera as Camera;
use dominator::clone;
use gloo_render::AnimationFrame;

use crate::error::EditorResult;

pub type RendererHandle = Arc<xutex::AsyncMutex<AwsmRenderer>>;
pub type CameraHandle = Arc<std::sync::Mutex<Camera>>;
pub type RenderHooksHandle = Arc<std::sync::RwLock<Option<RenderHooks>>>;
pub type WorkerPoolHandle = Arc<Option<WorkerPool>>;

pub fn with_canvas<T>(f: impl FnOnce(&web_sys::HtmlCanvasElement) -> T) -> T {
    APP_CONTEXT.with(|ctx| match ctx.get() {
        Some(ctx) => f(&ctx.canvas),
        None => panic!("AppContext not initialized when trying to access canvas"),
    })
}

pub async fn with_renderer_mut<T>(f: impl FnOnce(&mut AwsmRenderer) -> T) -> T {
    let handle = APP_CONTEXT.with(|ctx| ctx.get().map(|ctx| ctx.renderer.clone()));
    match handle {
        Some(handle) => {
            let mut renderer = handle.lock().await;
            f(&mut renderer)
        }
        None => panic!("AppContext not initialized when trying to access renderer"),
    }
}

pub fn with_camera_mut<T>(f: impl FnOnce(&mut Camera) -> T) -> T {
    APP_CONTEXT.with(|ctx| match ctx.get() {
        Some(ctx) => {
            let mut camera = ctx.camera.lock().unwrap();
            f(&mut camera)
        }
        None => panic!("AppContext not initialized when trying to access camera"),
    })
}

/// Fallible variant of [`with_camera_mut`] — returns `None` instead of panicking
/// when `create_context` hasn't completed yet (the canvas event listeners are
/// wired at mount time, before the async boot resolves).
pub fn try_with_camera_mut<T>(f: impl FnOnce(&mut Camera) -> T) -> Option<T> {
    APP_CONTEXT.with(|ctx| {
        ctx.get().map(|ctx| {
            let mut camera = ctx.camera.lock().unwrap();
            f(&mut camera)
        })
    })
}

pub fn set_raf(raf: AnimationFrame) {
    APP_CONTEXT.with(|ctx| match ctx.get() {
        Some(ctx) => *ctx.raf.lock().unwrap() = Some(raf),
        None => panic!("AppContext not initialized when trying to set tick"),
    });
}

/// Raw renderer handle for callers that hold the lock across awaits.
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

pub fn worker_pool_handle() -> WorkerPoolHandle {
    APP_CONTEXT.with(|ctx| {
        ctx.get()
            .expect("AppContext not initialized")
            .worker_pool
            .clone()
    })
}

/// True once `create_context` has populated the renderer context.
pub fn is_ready() -> bool {
    APP_CONTEXT.with(|ctx| ctx.get().is_some())
}

const GLTF_WORKER_POOL_SIZE: usize = 2;

/// Build a `WorkerPool` + register `GltfParseJob`. `None` on spawn failure
/// (CSP/blob restrictions) — asset loads degrade to the inline parse path.
async fn maybe_build_worker_pool() -> Option<WorkerPool> {
    match WorkerPool::new(WorkerPoolBootstrap::Auto, GLTF_WORKER_POOL_SIZE).await {
        Ok(pool) => {
            pool.register::<awsm_renderer_gltf::worker_job::GltfParseJob>();
            tracing::info!("WorkerPool built ({GLTF_WORKER_POOL_SIZE} workers)");
            Some(pool)
        }
        Err(err) => {
            tracing::warn!("WorkerPool bootstrap failed; inline glTF parse fallback: {err}");
            None
        }
    }
}

/// Boot the renderer context. Called once, when the canvas mounts.
pub async fn create_context(canvas: web_sys::HtmlCanvasElement) -> EditorResult<()> {
    // Renderer construction is the long pole; build the worker pool concurrently
    // so the boot critical path is `max(renderer, pool)` not the sum.
    let (renderer_result, worker_pool) =
        futures::future::join(create_renderer(canvas.clone()), maybe_build_worker_pool()).await;
    let renderer = renderer_result?;
    let renderer = Arc::new(xutex::AsyncMutex::new(renderer));
    let worker_pool: WorkerPoolHandle = Arc::new(worker_pool);

    let camera = {
        let mut cam = Camera::new_default_cube(16.0 / 9.0);
        cam.set_aperture(super::config::CONFIG.camera_aperture);
        cam.set_focus_distance(super::config::CONFIG.camera_focus_distance);
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

thread_local! {
    static APP_CONTEXT: OnceLock<AppContext> = OnceLock::new();
}

#[derive(Clone)]
struct AppContext {
    canvas: web_sys::HtmlCanvasElement,
    renderer: RendererHandle,
    camera: CameraHandle,
    resize_observer: Arc<ResizeObserver>,
    render_hooks: RenderHooksHandle,
    raf: Arc<std::sync::Mutex<Option<AnimationFrame>>>,
    worker_pool: WorkerPoolHandle,
    _drop_tracker: Arc<AppContextDropTracker>,
}

fn editor_features() -> RendererFeatures {
    use awsm_renderer::features::FeatureToggle;
    RendererFeatures {
        gpu_culling: true,
        decals: true,
        coverage_lod: false,
        // The canvas wires `.pick()` to pointer-down for node selection (M6).
        picking: true,
        indirect_first_instance: FeatureToggle::Auto,
    }
}

async fn create_renderer(canvas: web_sys::HtmlCanvasElement) -> EditorResult<AwsmRenderer> {
    let gpu = web_sys::window().unwrap().navigator().gpu();
    let gpu_builder = AwsmRendererWebGpuBuilder::new(gpu, canvas)
        .with_configuration(
            CanvasConfiguration::default()
                .with_alpha_mode(CanvasAlphaMode::Opaque)
                .with_tone_mapping(CanvasToneMappingMode::Standard)
                // RENDER_ATTACHMENT (to draw) + COPY_SRC so the WebGPU swapchain
                // is readable via `toDataURL`/`drawImage` — Chrome returns an
                // empty (transparent) buffer for a WebGPU canvas without
                // COPY_SRC, which is what made `screenshot_scene`/`canvas_stats`
                // come back blank while the scene rendered fine on screen.
                .with_usage(
                    awsm_renderer_core::texture::TextureUsage::new()
                        .with_render_attachment()
                        .with_copy_src(),
                ),
        )
        .with_device_request_limits(DeviceRequestLimits::max_all());

    // Editor forces the GPU-driven path so it's exercised during authoring
    // regardless of scene size (Auto would park it off below ~500 meshes).
    let policy = awsm_renderer::optimization_policy::RendererOptimizationPolicy {
        gpu_culling: awsm_renderer::optimization_policy::OptimizationMode::Force,
        ..Default::default()
    };
    let profile = awsm_web_shared::perf::resolve_renderer_profile(
        awsm_renderer::profile::RendererProfile::Desktop,
    );
    let renderer = AwsmRendererBuilder::new(gpu_builder)
        .with_profile(profile)
        .with_logging(AwsmRendererLogging {
            render_timings: awsm_web_shared::perf::resolve_render_timings(
                if cfg!(debug_assertions) {
                    awsm_renderer::debug::RenderTimings::SubFrame
                } else {
                    awsm_renderer::debug::RenderTimings::Frame
                },
            ),
        })
        .with_clear_color(Color::MID_GREY)
        .with_features(editor_features())
        .with_optimization_policy(policy)
        .with_phase_handler(|phase| {
            let msg = match phase {
                awsm_renderer::RendererLoadingPhase::Init => "Initializing renderer",
                awsm_renderer::RendererLoadingPhase::CompilingShaders => {
                    "Browser is compiling shaders (first load may take a while)"
                }
                awsm_renderer::RendererLoadingPhase::BuildingPipelines => {
                    "Building render pipelines"
                }
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
                    renderer.gpu.sync_canvas_buffer_with_css();
                    let camera_matrices = {
                        let mut camera = camera.lock().unwrap();
                        camera.set_aspect(width as f32 / height as f32);
                        camera.matrices()
                    };
                    if let Err(err) = renderer.update_camera(camera_matrices) {
                        tracing::error!("camera update on resize: {err:?}");
                    }
                    let hooks = render_hooks.read().unwrap();
                    if let Err(err) = renderer.render(hooks.as_ref()) {
                        tracing::error!("render on resize: {err:?}");
                    }
                }
            }));
        },
        None,
    );
    resize_observer.observe(&canvas);
    resize_observer
}

/// Explicitly size the WebGPU surface to the canvas's current client box.
///
/// The `ResizeObserver` does not reliably deliver an initial callback when the
/// canvas is reparented into the viewport slot *after* layout, so on first mount
/// the surface would otherwise stay at the default 300×150 — a low-res, upscaled
/// render *and* a GPU pick id-buffer too small for CSS-space click coordinates
/// (every pick clamps + misses). The viewport calls this once on mount; it polls
/// a few frames for the slot to acquire a real size, then applies the same
/// resize the observer would.
pub fn sync_canvas_size() {
    thread_local! {
        // At most ONE resize task in flight at a time. The viewport's
        // `after_inserted` can fire repeatedly (DOM rebuilds on signal changes),
        // and a mode switch re-invokes this on the reparent; concurrent resize
        // tasks race each other's render-texture recreation and produce
        // "destroyed texture in submit" GPU errors. Overlapping calls are
        // coalesced — but, unlike a run-once latch, the flag clears when the
        // task finishes, so a later reparent into a differently-sized slot
        // (e.g. Scene ⇄ Animation, whose viewports differ in size) still resizes
        // the surface. The `ResizeObserver` handles steady-state resizes; this
        // is the backstop for reparents it doesn't reliably deliver.
        static IN_FLIGHT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if IN_FLIGHT.with(|d| d.replace(true)) {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        for _ in 0..30 {
            let (cw, ch) = with_canvas(|c| (c.client_width(), c.client_height()));
            if cw > 0 && ch > 0 {
                let (width, height) = (cw as u32, ch as u32);
                let renderer_handle = renderer_handle();
                let camera_handle = camera_handle();
                let mut renderer = renderer_handle.lock().await;
                renderer.gpu.canvas().set_width(width);
                renderer.gpu.canvas().set_height(height);
                renderer.gpu.sync_canvas_buffer_with_css();
                // Set the real aspect, then draw one frame immediately (same as the
                // ResizeObserver path). The RAF loop also renders every frame, but
                // on first mount a render hook installed asynchronously after boot
                // (e.g. the viewport grid, whose pipelines compile off-thread) could
                // otherwise stay invisible until a manual resize triggers the
                // observer's render. We hold the renderer lock across the
                // reconfigure + render, and the RAF loop uses `try_lock` (skips while
                // we hold it), so there's no in-flight-submit race against the
                // texture recreation.
                let camera_matrices = {
                    let mut camera = camera_handle.lock().unwrap();
                    camera.set_aspect(width as f32 / height as f32);
                    camera.matrices()
                };
                if let Err(err) = renderer.update_camera(camera_matrices) {
                    tracing::error!("camera update on canvas sync: {err:?}");
                }
                let hooks = render_hooks_handle();
                let hooks = hooks.read().unwrap();
                if let Err(err) = renderer.render(hooks.as_ref()) {
                    tracing::error!("render on canvas sync: {err:?}");
                }
                IN_FLIGHT.with(|d| d.set(false));
                return;
            }
            gloo_timers::future::TimeoutFuture::new(16).await;
        }
        // The slot never acquired a real size; release so a later call can retry.
        IN_FLIGHT.with(|d| d.set(false));
    });
}

struct AppContextDropTracker;

impl Drop for AppContextDropTracker {
    fn drop(&mut self) {
        tracing::error!("AppContext dropped!");
    }
}
