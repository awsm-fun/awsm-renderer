//! Material-editor — standalone authoring tool for runtime-registered
//! custom materials.
//!
//! See `docs/plans/dynamic-materials.md` for the architectural rationale.
//! UI is built with dominator + futures-signals, mirroring scene-editor.
//!
//! ## Architecture
//!
//! - `state.rs` owns the `EditState` (definition + WGSL + errors)
//!   exposed to the UI panes via dominator signals.
//! - `panes/` renders the four-pane layout — definition / WGSL /
//!   contract / preview+errors.
//! - `recompile.rs` orchestrates the debounced edit-to-recompile loop
//!   via the host-agnostic `RecompileSink` trait.
//! - `host.rs` implements `RecompileSink` over a shared
//!   `RendererHandle` (a `Rc<RefCell<Option<RendererHost>>>`) that
//!   wraps the live `AwsmRenderer`.
//! - `main.rs` (here) is the entry point: mounts the UI, boots the
//!   renderer asynchronously, and spawns the recompile loop once
//!   both are ready.

mod app;
mod host;
mod panes;
mod recompile;
mod state;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use futures_signals::signal::Mutable;
use wasm_bindgen_futures::spawn_local;

use crate::host::{RendererHandle, RendererHost, RendererRecompileSink};
use crate::recompile::RecompileSink;
use crate::state::EditState;

fn main() {
    awsm_web_shared::logger::init_logger();
    spawn_local(async {
        if let Err(err) = run().await {
            tracing::error!("[material-editor] init failed: {err:?}");
        }
    });
}

async fn run() -> anyhow::Result<()> {
    let body = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
        .ok_or_else(|| anyhow::anyhow!("no <body>"))?;

    // Mount the UI first so the user sees the editor immediately
    // (even while the renderer is still booting in the background).
    let state = EditState::new_scanline();
    dominator::append_dom(&body, app::root_with_state(state.clone()));

    // Stub the renderer handle. The boot future below populates it.
    let renderer_handle: RendererHandle = Rc::new(RefCell::new(None));

    // Spawn the debounced-recompile loop driven by edits to
    // `state.definition` / `state.wgsl_source`. Its sink is the
    // renderer host — registrations land via
    // `AwsmRenderer::register_material`.
    let sink: Arc<Mutable<Box<dyn RecompileSink>>> = Arc::new(Mutable::new(Box::new(
        RendererRecompileSink::new(renderer_handle.clone()),
    )));
    recompile::spawn(state.clone(), sink);

    // Boot the renderer asynchronously. On success the handle's
    // RefCell flips to Some; the recompile sink starts producing
    // real registrations.
    spawn_local(async move {
        match boot_renderer().await {
            Ok(host) => {
                *renderer_handle.borrow_mut() = Some(host);
                tracing::info!("[material-editor] renderer ready");
            }
            Err(e) => {
                tracing::error!("[material-editor] renderer boot failed: {e:?}");
            }
        }
    });

    Ok(())
}

/// Build an `AwsmRenderer` against the preview canvas.
///
/// Polls for the canvas (dominator mounts asynchronously), then
/// constructs the renderer via `AwsmRendererBuilder`. The actual
/// stub-scene mesh + RAF loop is started by the caller (`run()`)
/// once the host is populated.
async fn boot_renderer() -> anyhow::Result<RendererHost> {
    use awsm_renderer::AwsmRendererBuilder;
    use awsm_renderer_core::configuration::{
        CanvasAlphaMode, CanvasConfiguration, CanvasToneMappingMode,
    };
    use awsm_renderer_core::renderer::{AwsmRendererWebGpuBuilder, DeviceRequestLimits};
    use gloo_timers::future::TimeoutFuture;
    use wasm_bindgen::JsCast;

    // Poll for the canvas with an 800 ms budget.
    let mut canvas: Option<web_sys::HtmlCanvasElement> = None;
    for _ in 0..50 {
        if let Some(elem) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("preview-canvas"))
        {
            if let Ok(c) = elem.dyn_into::<web_sys::HtmlCanvasElement>() {
                canvas = Some(c);
                break;
            }
        }
        TimeoutFuture::new(16).await;
    }
    let canvas = canvas.ok_or_else(|| anyhow::anyhow!("preview-canvas not in DOM after 800ms"))?;

    let gpu = web_sys::window()
        .ok_or_else(|| anyhow::anyhow!("no window"))?
        .navigator()
        .gpu();
    let gpu_builder = AwsmRendererWebGpuBuilder::new(gpu, canvas)
        .with_configuration(
            CanvasConfiguration::default()
                .with_alpha_mode(CanvasAlphaMode::Opaque)
                .with_tone_mapping(CanvasToneMappingMode::Standard),
        )
        .with_device_request_limits(DeviceRequestLimits::max_all());

    let renderer = AwsmRendererBuilder::new(gpu_builder)
        .build()
        .await
        .map_err(|e| anyhow::anyhow!("AwsmRendererBuilder::build failed: {e:?}"))?;

    Ok(RendererHost::new(renderer))
}
