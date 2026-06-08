//! awsm-editor — v2 blank-slate rebuild bootstrap.
//!
//! Boots the real WebGPU renderer (the multi-second cold-start window is covered
//! by the HTML boot-loader, captioned by the renderer's phase handler), then
//! mounts the app shell once the context is ready. The `EditorController` is
//! installed before any UI so every panel dispatches through it.

mod animation_mode;
mod app;
mod command_palette;
mod controller;
mod engine;
mod error;
mod fs;
mod material_mode;
mod prelude;
mod remote;
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

    // Establish the command/query authority before mounting any UI: every later
    // panel dispatches through this singleton.
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
                            awsm_web_shared::util::window::set_boot_loader_message("Compiling render pipelines…");
                            {
                                let handle = engine::context::renderer_handle();
                                let mut r = handle.lock().await;
                                // Surface the live compile count on the boot
                                // loader so first-start pipeline creation is
                                // visible (mirrors the in-app pill that covers
                                // post-mount import/material compiles).
                                let on_progress = |p: awsm_renderer::pipeline_scheduler::CompileProgress| {
                                    let n = p.in_flight_subcompiles;
                                    if n > 0 {
                                        awsm_web_shared::util::window::set_boot_loader_message(&format!(
                                            "Compiling render pipelines… ({n} remaining)"
                                        ));
                                    }
                                };
                                if let Err(err) = r.wait_for_pipelines_ready_with_progress(on_progress).await {
                                    tracing::warn!("wait_for_pipelines_ready: {err}");
                                }
                            }
                            // Mirror the scene onto the renderer (materializes
                            // any already-present nodes + every future insert).
                            engine::bridge::init();
                            engine::render_loop::start();
                            // Compile the GPU picker in the background so the
                            // first viewport click selects without a warm-up miss.
                            engine::canvas::prewarm_picker();
                            // Viewport ground grid (toggled by Settings → Show grid).
                            engine::grid::init();
                            // Transform gizmo (loads gizmo.glb, anchors on selection).
                            engine::gizmo::init();
                            // Per-control-point drag handles for selected curves.
                            engine::curve_handles::init();
                            // Push view settings (MSAA / light-heatmap) to the renderer.
                            engine::settings_sync::start();
                            ctx_ready.set(true);
                            awsm_web_shared::util::window::remove_boot_loader();
                            // Gesture-free project load: `?load=<base_url>` auto-loads
                            // a project on boot. The scriptable / MCP entry point — the
                            // gesture-free `LoadProjectFromUrl` otherwise has no trigger.
                            if let Some(base) = boot_load_url() {
                                spawn_local(async move {
                                    if let Err(e) = controller::controller()
                                        .dispatch(controller::EditorCommand::LoadProjectFromUrl { base_url: base })
                                        .await
                                    {
                                        tracing::error!("?load auto-load failed: {e}");
                                    }
                                });
                            }
                            // Remote MCP control: `?mcp=<control-origin>` auto-dials
                            // the native server over WebTransport. Absent → the
                            // top-bar MCP button connects on demand (to the dev
                            // default origin).
                            if let Some(origin) = boot_mcp_origin() {
                                remote::connect(origin);
                            }
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

/// Read a `?load=<base_url>` query parameter (URL-decoded) for the gesture-free
/// boot-time project load. Returns `None` when absent.
fn boot_load_url() -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let q = search.strip_prefix('?').unwrap_or(&search);
    for pair in q.split('&') {
        if let Some(val) = pair.strip_prefix("load=") {
            let decoded = js_sys::decode_uri_component(val)
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| val.to_string());
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

/// Read a `?mcp=<control-origin>` query parameter (URL-decoded) — the native MCP
/// server's HTTP control origin (e.g. `http://127.0.0.1:9086`). Returns `None`
/// when absent (remote control disabled).
fn boot_mcp_origin() -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let q = search.strip_prefix('?').unwrap_or(&search);
    for pair in q.split('&') {
        if let Some(val) = pair.strip_prefix("mcp=") {
            let decoded = js_sys::decode_uri_component(val)
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| val.to_string());
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

/// External-inspection seam: a JS-callable export returning the
/// serializable editor snapshot as JSON. This is exactly what a future
/// MCP/websocket transport (or a headless test driving the build) reads back —
/// the transport itself is NOT built now, only this read seam.
#[wasm_bindgen]
pub fn editor_snapshot_json() -> String {
    controller::controller().snapshot_json()
}

/// External-dispatch seam: decode a JSON `EditorCommand` and dispatch it
/// through the controller. This is the write half of the future MCP transport
/// (decode command → dispatch); built now only as the seam + for scriptable,
/// gesture-free testing. Returns `"ok"` on a valid decode (dispatch is async and
/// spawned) or a decode error.
/// Serialize the live project to `project.toml` (the persistence seam — used by
/// the Save writer + headless round-trip tests).
#[wasm_bindgen]
pub fn editor_project_toml() -> String {
    controller::persistence::project_to_toml(&controller::controller())
        .unwrap_or_else(|e| format!("# error: {e}"))
}

/// The current workspace mode (`"scene"` | `"material"`) — lets a driver pick
/// which image query to take.
#[wasm_bindgen]
pub fn editor_query_mode() -> String {
    match controller::controller().mode.get() {
        controller::EditorMode::Scene => "scene".to_string(),
        controller::EditorMode::Material => "material".to_string(),
        controller::EditorMode::Animation => "animation".to_string(),
    }
}

/// PNG data URL of the scene viewport (through the active camera). Empty string
/// if the canvas isn't ready.
#[wasm_bindgen]
pub fn editor_query_scene_png() -> String {
    engine::query::scene_png(None, None).unwrap_or_default()
}

/// PNG data URL of the material-mode preview sphere. Empty string if the Studio
/// isn't mounted.
#[wasm_bindgen]
pub fn editor_query_material_png() -> String {
    engine::query::material_png(None, None).unwrap_or_default()
}

/// PNG data URL of a texture asset (by UUID) — procedural textures are encoded
/// directly, raster/file textures are read back from the GPU. Async (the GPU
/// readback maps a buffer); returns `"error: …"` on failure. Callers `await` it.
#[wasm_bindgen]
pub async fn editor_query_texture_png(asset_id: &str) -> String {
    match engine::query::parse_asset_id(asset_id) {
        Some(id) => engine::query::texture_png(id)
            .await
            .unwrap_or_else(|e| format!("error: {e}")),
        None => "error: invalid asset id".to_string(),
    }
}

/// Animation/verification read seam: decode a JSON `EditorQuery`, run it
/// through `controller().query(...)`, and return the JSON result. Async because
/// the value/pixel readbacks await the renderer lock (mirrors
/// `editor_query_texture_png`). The read half of the future MCP transport.
#[wasm_bindgen]
pub async fn editor_query_json(query_json: String) -> String {
    controller::controller().query_json(&query_json).await
}

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
