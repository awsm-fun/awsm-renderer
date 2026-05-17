#![allow(
    dead_code,
    clippy::arc_with_non_send_sync,
    clippy::manual_map,
    clippy::missing_const_for_thread_local
)]

use std::sync::{Arc, OnceLock};

use awsm_renderer::{
    debug::AwsmRendererLogging, render::RenderHooks, AwsmRenderer, AwsmRendererBuilder,
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

// Called once at init
pub async fn create_context(canvas: web_sys::HtmlCanvasElement) -> EditorResult<()> {
    let renderer = create_renderer(canvas.clone()).await?;
    let renderer = Arc::new(xutex::AsyncMutex::new(renderer));

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

    // just for debugging
    _drop_tracker: Arc<AppContextDropTracker>,
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

    let renderer = AwsmRendererBuilder::new(gpu_builder)
        .with_logging(AwsmRendererLogging {
            render_timings: cfg!(debug_assertions),
        })
        .with_clear_color(Color::MID_GREY)
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
