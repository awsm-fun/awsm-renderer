//! awsm-editor — v2 blank-slate rebuild bootstrap.
//!
//! Boots the real WebGPU renderer (the multi-second cold-start window is covered
//! by the HTML boot-loader, captioned by the renderer's phase handler), then
//! mounts the app shell once the context is ready. The `EditorController` is
//! installed before any UI so every panel dispatches through it.

mod app;
mod controller;
mod engine;
mod error;
mod prelude;
mod scene_mode;

use awsm_web_shared::{logger, prelude::*, theme};
use dominator::stylesheet;
use wasm_bindgen_futures::spawn_local;

pub fn main() {
    // Register every WorkerJob the editor wants available — runs on both the
    // main thread and the pool workers (which re-run this same wasm `main`).
    awsm_renderer::workers::register_job::<awsm_renderer_gltf::worker_job::GltfParseJob>();

    // Worker context: `awsm_worker_entry` is invoked separately by the bootstrap
    // JS; bail before any DOM-touching setup if there's no Window.
    if web_sys::window().is_none() {
        return;
    }

    awsm_web_shared::util::window::set_boot_loader_message("Initializing renderer");
    logger::init_logger();
    Modal::init_panic_hook();
    theme::stylesheet::init();

    stylesheet!("html, body", {
        .style("width", "100%")
        .style("height", "100%")
    });
    stylesheet!("body", {
        .style(["-moz-user-select", "user-select", "-webkit-user-select"], "none")
    });
    stylesheet!("input, textarea, [contenteditable='true']", {
        .style(["-moz-user-select", "user-select", "-webkit-user-select"], "text")
    });

    // Establish the command/query authority before mounting any UI (decision 8):
    // every later panel dispatches through this singleton.
    controller::init();

    let ctx_ready = Mutable::new(false);

    dominator::append_dom(
        &dominator::body(),
        html!("div", {
            .style("width", "100%")
            .style("height", "100%")
            // Suppress the browser's native right-click menu everywhere; surfaces
            // open their own context menus. `preventable` is required for
            // `prevent_default` to take effect.
            .event_with_options(&dominator::EventOptions::preventable(), |event: events::ContextMenu| {
                event.prevent_default();
            })
            // Global overlay hosts (mounted before ctx_ready so the panic hook
            // and early toasts have somewhere to surface).
            .child(Modal::render())
            .child(Toast::render())
            // The WebGPU canvas is created here (triggering create_context); the
            // Scene workspace reparents it into the viewport slot once mounted.
            .child(engine::canvas::render_canvas(clone!(ctx_ready => move |canvas| {
                spawn_local(clone!(ctx_ready => async move {
                    match engine::context::create_context(canvas).await {
                        Ok(_) => {
                            awsm_web_shared::util::window::set_boot_loader_message("Warming pipelines");
                            {
                                let handle = engine::context::renderer_handle();
                                let mut r = handle.lock().await;
                                if let Err(err) = r.wait_for_pipelines_ready().await {
                                    tracing::warn!("wait_for_pipelines_ready: {err}");
                                }
                            }
                            // Mirror the scene onto the renderer (materializes
                            // any already-present nodes + every future insert).
                            engine::bridge::init();
                            engine::render_loop::start();
                            ctx_ready.set(true);
                            awsm_web_shared::util::window::remove_boot_loader();
                        }
                        Err(err) => {
                            awsm_web_shared::util::window::remove_boot_loader();
                            Modal::error(format!("Failed to initialize renderer: {err}"));
                        }
                    }
                }));
            })))
            .child_signal(ctx_ready.signal().map(|ready| if ready { Some(app::render()) } else { None }))
        }),
    );
}

/// External-inspection seam (§5.5): a JS-callable export returning the
/// serializable editor snapshot as JSON. This is exactly what a future
/// MCP/websocket transport (or a headless test driving the build) reads back —
/// the transport itself is NOT built now, only this read seam.
#[wasm_bindgen]
pub fn editor_snapshot_json() -> String {
    controller::controller().snapshot_json()
}

/// External-dispatch seam (§5.5): decode a JSON `EditorCommand` and dispatch it
/// through the controller. This is the write half of the future MCP transport
/// (decode command → dispatch); built now only as the seam + for scriptable,
/// gesture-free testing. Returns `"ok"` on a valid decode (dispatch is async and
/// spawned) or a decode error.
#[wasm_bindgen]
pub fn editor_dispatch_json(cmd_json: &str) -> String {
    match serde_json::from_str::<controller::EditorCommand>(cmd_json) {
        Ok(cmd) => {
            spawn_local(async move {
                if let Err(err) = controller::controller().dispatch(cmd).await {
                    tracing::error!("dispatch failed: {err}");
                }
            });
            "ok".to_string()
        }
        Err(err) => format!("decode error: {err}"),
    }
}
