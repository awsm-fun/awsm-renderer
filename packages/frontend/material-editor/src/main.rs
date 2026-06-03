//! Material-editor — standalone authoring tool for runtime-registered
//! custom materials.
//!
//! The author-facing WGSL contract is in
//! `docs/dynamic-materials/contract-{opaque,transparent}.md`. UI is
//! built with dominator + futures-signals, mirroring scene-editor.
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
    // Read the ?folder=<name> deep-link param. Stored on EditState;
    // the banner shows when it's Some, and clicking its button
    // triggers the FS Access API picker. No-op when the param is
    // absent.
    if let Some(folder) = panes::deep_link_banner::read_folder_query_param() {
        state.deep_link_folder.set(Some(folder));
    }
    dominator::append_dom(&body, app::root_with_state(state.clone()));

    // Stub the renderer handle. The boot future below populates it.
    let renderer_handle: RendererHandle = Rc::new(RefCell::new(None));

    // Spawn the debounced-recompile loop driven by edits to
    // `state.definition` / `state.wgsl_source`. Its sink is the
    // renderer host — registrations land via
    // `AwsmRenderer::register_material`. Single-threaded wasm runtime
    // means Rc + Mutable's interior mutability are sufficient
    // (clippy flags Arc<!Send> otherwise).
    let sink: Rc<Mutable<Box<dyn RecompileSink>>> = Rc::new(Mutable::new(Box::new(
        RendererRecompileSink::new(renderer_handle.clone(), state.preview_mesh.clone()),
    )));
    recompile::spawn(state.clone(), sink);

    // Boot the renderer asynchronously. On success the handle's
    // RefCell flips to Some; the recompile sink starts producing
    // real registrations, AND the RAF loop starts driving the GPU
    // each frame.
    //
    // The recompile loop's very first trigger fired *before* the
    // renderer was ready (the sink saw `None` and silently returned
    // Ok). Without an explicit kick here the editor would sit with
    // no registered material until the user typed something. We push
    // a bump to `wgsl_source` (rewriting it to itself) so the
    // signal re-fires and the debounced loop registers the initial
    // scanline material.
    let renderer_handle_for_boot = renderer_handle.clone();
    let state_for_boot = state.clone();
    spawn_local(async move {
        match boot_renderer().await {
            Ok(host) => {
                *renderer_handle_for_boot.borrow_mut() = Some(host);
                tracing::info!("[material-editor] renderer ready");
                start_render_loop(renderer_handle_for_boot.clone(), state_for_boot.clone());
                // Re-fire the WGSL signal so the debounced recompile
                // loop registers the initial material now that the
                // renderer exists. `Mutable::set` triggers the
                // signal even when the value is structurally equal,
                // so this is sufficient.
                let current = state_for_boot.wgsl_source.lock_ref().clone();
                state_for_boot.wgsl_source.set(current);
            }
            Err(e) => {
                tracing::error!("[material-editor] renderer boot failed: {e:?}");
            }
        }
    });

    Ok(())
}

/// Per-frame render-loop driver. Kicks off `requestAnimationFrame` and
/// re-schedules itself each tick. Reads the live renderer through the
/// shared handle; if a registration is in flight (the handle's
/// RefCell is borrowed mutably by the recompile sink) we skip a
/// frame and try again next tick.
///
/// Each frame:
///   1. Refresh the camera matrices (fixed view looking at the quad).
///   2. Flush dirty transforms to the GPU.
///   3. Issue the render.
///
/// The camera is recomputed every frame so the renderer's
/// `last_matrices`-based dirty tracking marks the camera buffer dirty
/// during the initial frames — without this the very first frame
/// renders against zero matrices and the quad never appears.
fn start_render_loop(handle: RendererHandle, state: EditState) {
    use awsm_renderer::camera::CameraMatrices;
    use awsm_renderer::pipeline_scheduler::PipelineGroupStatus;
    use glam::{Mat4, Vec3};
    use std::cell::RefCell;
    use std::rc::Rc;

    let raf_holder: Rc<RefCell<Option<gloo_render::AnimationFrame>>> = Rc::new(RefCell::new(None));

    fn tick(
        handle: RendererHandle,
        state: EditState,
        raf_holder: Rc<RefCell<Option<gloo_render::AnimationFrame>>>,
    ) {
        if let Ok(mut guard) = handle.try_borrow_mut() {
            if let Some(host) = guard.as_mut() {
                // Block A.4: drain pipeline status events into the
                // editor's compile_pending counter. Pending → +1,
                // Ready/Failed → -1 (saturating). Failed events also
                // surface their error string in the modal's "Last
                // error" subsection. Cleared on the leading edge of a
                // fresh compile batch (prev_pending == 0 transitioning
                // to >0) so the next batch starts clean.
                let events = host.renderer.drain_pipeline_status_events();
                if !events.is_empty() {
                    let prev_pending = state.compile_pending.get();
                    let mut pending = prev_pending;
                    let mut latest_err: Option<String> = None;
                    let mut opened_new_batch = false;
                    for ev in events {
                        match ev.status {
                            PipelineGroupStatus::Pending => {
                                if pending == 0 {
                                    opened_new_batch = true;
                                }
                                pending = pending.saturating_add(1);
                            }
                            PipelineGroupStatus::Ready => {
                                pending = pending.saturating_sub(1);
                            }
                            PipelineGroupStatus::Failed { error: _ } => {
                                // The event's `error` is intentionally
                                // a placeholder (`PipelineVariantNotCompiled
                                // ("see scheduler state")`) — the real
                                // failure detail lives on the
                                // scheduler's material/pass state. Query
                                // it back via `pipeline_group_status`.
                                pending = pending.saturating_sub(1);
                                let err_msg = match host.renderer.pipeline_group_status(ev.id) {
                                    Some(PipelineGroupStatus::Failed { error: real }) => {
                                        format!("{real}")
                                    }
                                    _ => "compile failed (status no longer queryable)".to_string(),
                                };
                                latest_err = Some(err_msg);
                            }
                        }
                    }
                    state.compile_pending.set(pending);
                    if let Some(msg) = latest_err {
                        state.compile_last_error.set(Some(msg));
                    } else if opened_new_batch && prev_pending == 0 {
                        state.compile_last_error.set(None);
                    }
                }

                // Camera: looking at the quad at (0, 0, -3) from
                // slightly above + back so the preview shows the
                // material in a 3/4 view. Aspect is left at 1:1
                // since the preview canvas is square; if the canvas
                // gets resized later, plumb the actual canvas
                // dimensions through here.
                let eye = Vec3::new(0.0, 0.5, 1.5);
                let target = Vec3::new(0.0, 0.0, -3.0);
                let view = Mat4::look_at_rh(eye, target, Vec3::Y);
                // Aspect matches the fixed 800×600 preview canvas.
                let projection =
                    Mat4::perspective_rh(60.0_f32.to_radians(), 800.0 / 600.0, 0.1, 100.0);
                if let Err(e) = host.renderer.update_camera(CameraMatrices {
                    view,
                    projection,
                    position_world: eye,
                    focus_distance: (target - eye).length(),
                    aperture: 5.6,
                }) {
                    tracing::warn!("[material-editor] update_camera failed: {e:?}");
                }
                host.renderer.update_transforms();

                if let Err(e) = host.renderer.render(None) {
                    tracing::warn!("[material-editor] render failed: {e:?}");
                }
            }
        }
        let next_handle = handle.clone();
        let next_state = state.clone();
        let next_holder = raf_holder.clone();
        let new_raf = gloo_render::request_animation_frame(move |_ts| {
            tick(next_handle.clone(), next_state.clone(), next_holder.clone());
        });
        *raf_holder.borrow_mut() = Some(new_raf);
    }

    tick(handle, state, raf_holder);
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

    // Mid-grey clear so an empty preview (no registered material yet,
    // or quad not rasterizing for whatever reason) is visibly
    // distinct from "render never ran" — matches the scene-editor's
    // convention. Profile defaults to Desktop with `?mobile=true`
    // override available.
    let profile = awsm_web_shared::perf::resolve_renderer_profile(
        awsm_renderer::profile::RendererProfile::Desktop,
    );
    let renderer = AwsmRendererBuilder::new(gpu_builder)
        .with_profile(profile)
        .with_clear_color(awsm_renderer_core::command::color::Color::MID_GREY)
        .build()
        .await
        .map_err(|e| anyhow::anyhow!("AwsmRendererBuilder::build failed: {e:?}"))?;

    Ok(RendererHost::new(renderer))
}
