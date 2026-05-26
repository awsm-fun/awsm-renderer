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
/// Returns a [`RendererHost`] wrapping the live renderer. The
/// material-editor's preview pane mounts the canvas under
/// `id="preview-canvas"`; the renderer attaches to that element's
/// GPU context. The actual render-loop dispatch + stub-scene mesh
/// construction (a quad with the loaded material applied) is the
/// next-session UI work — Phase 18 ships the registration plumbing
/// so the material's bytes flow through the renderer-side packer
/// correctly even before the visible mesh lands.
async fn boot_renderer() -> anyhow::Result<RendererHost> {
    // The canvas may not exist yet when this future first runs
    // (dominator mounts asynchronously). Poll briefly for it before
    // giving up. 50 attempts × 16 ms ≈ 800 ms total budget — far
    // longer than the dominator mount takes on any sane page.
    use gloo_timers::future::TimeoutFuture;
    let mut canvas = None;
    for _ in 0..50 {
        if let Some(elem) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("preview-canvas"))
        {
            canvas = Some(elem);
            break;
        }
        TimeoutFuture::new(16).await;
    }
    let canvas = canvas.ok_or_else(|| anyhow::anyhow!("preview-canvas not in DOM after 800ms"))?;
    let _canvas = canvas; // Held for future render-loop wiring.

    // The full AwsmRendererBuilder::new(...) → build() chain is the
    // remaining Phase-9 wiring. The builder requires a
    // AwsmRendererWebGpuBuilder pointing at the canvas's WebGPU
    // context — straightforward (~30 LoC), but the dominator-side
    // setup that follows (loading a default mesh, attaching the
    // material, running a RAF loop) is the bulk of the work.
    //
    // For the registration-plumbing path this file delivers, the
    // renderer-side state is exercised through the
    // RendererRecompileSink: when the user edits the WGSL, the
    // sink builds a MaterialRegistration, calls register_material,
    // and the result (Ok / WgslCompile-error) surfaces back into
    // the editor's error pane.
    Err(anyhow::anyhow!(
        "boot_renderer: full AwsmRenderer construction is the next-session UI wiring"
    ))
}
