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
mod help_modal;
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
                                // Surface the live per-phase progress on the boot
                                // loader so first-start geometry upload / texture
                                // finalize / pipeline creation are each visible
                                // (mirrors the in-app pill + the model-tests overlay;
                                // the shared `LoadingStats::phase_label()` keeps the
                                // wording identical across all three).
                                let on_progress = |stats: awsm_renderer::LoadingStats| {
                                    if let Some(label) = stats.phase_label() {
                                        awsm_web_shared::util::window::set_boot_loader_message(&label);
                                    }
                                };
                                // Boot commit: opens the render gate (the editor
                                // never calls begin_load for its steady state, so
                                // this first commit_load is what flips
                                // scene_committed true) and compiles the initial
                                // scene's pipelines. The one compile path.
                                if let Err(err) = r.commit_load(on_progress).await {
                                    tracing::warn!("boot commit_load: {err}");
                                }
                            }
                            // Mirror the scene onto the renderer (materializes
                            // any already-present nodes + every future insert).
                            engine::bridge::init();
                            // Apply the environment skybox/IBL synchronously
                            // BEFORE the first paint — the renderer's default
                            // skybox is black, and the async observer in
                            // `env_sync::start` only reflects after a later
                            // bind-group rebuild (black sky on cold start).
                            engine::bridge::env_sync::apply_initial().await;
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
                            // Pickable HUD icons so lights are selectable in the viewport.
                            engine::light_icons::init();
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
                            // the native server over a WebSocket, optionally with
                            // `&pair=<code>` to claim a specific agent when more
                            // than one tab/agent is connected. Absent → the top-bar
                            // MCP button connects on demand (to the dev default).
                            if let Some(pair) = boot_mcp_pair() {
                                remote::set_pair_code(pair);
                            }
                            if let Some(origin) = boot_mcp_origin() {
                                remote::connect(origin);
                            }
                        }
                        Err(err) => {
                            awsm_web_shared::util::window::remove_boot_loader();
                            let msg = format!("Failed to initialize renderer: {err}");
                            // Agent observability: a boot failure happens BEFORE
                            // any MCP attach, so without this beacon the failure
                            // is invisible outside the browser console (every
                            // /debug request just times out). Fire-and-forget
                            // POST to the relay's /boot-error; agents read it
                            // back via GET /health.
                            if let Some(origin) = boot_mcp_origin() {
                                report_boot_error(&origin, &msg);
                            }
                            Modal::error(msg);
                        }
                    }
                }));
            })))
            .child_signal(ctx_ready.signal().map(|ready| if ready { Some(app::render()) } else { None }))
        }),
    );
}

/// Fire-and-forget POST of a boot-failure message to the MCP relay's
/// `/boot-error` endpoint (see the Err arm above — agent observability).
fn report_boot_error(origin: &str, msg: &str) {
    let url = format!("{}/boot-error", origin.trim_end_matches('/'));
    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_body(&wasm_bindgen::JsValue::from_str(msg));
    if let (Some(win), Ok(req)) = (
        web_sys::window(),
        web_sys::Request::new_with_str_and_init(&url, &opts),
    ) {
        // Ignore the response entirely — best-effort beacon.
        let _ = win.fetch_with_request(&req);
    }
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
    boot_query_param("mcp")
}

/// Read a `?pair=<code>` query parameter (URL-decoded) — the pairing code that
/// claims a specific MCP agent when more than one tab/agent is connected.
fn boot_mcp_pair() -> Option<String> {
    boot_query_param("pair")
}

/// Read a `<key>=<value>` query parameter (URL-decoded) from the page URL.
fn boot_query_param(key: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let q = search.strip_prefix('?').unwrap_or(&search);
    let prefix = format!("{key}=");
    for pair in q.split('&') {
        if let Some(val) = pair.strip_prefix(&prefix) {
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
/// if the canvas isn't ready. Async (returns a JS Promise) — the scene is read
/// back from the GPU on the next presented frame.
#[wasm_bindgen]
pub async fn editor_query_scene_png() -> String {
    engine::query::scene_png(None, None)
        .await
        .unwrap_or_default()
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

/// Test seam: advance the renderer's animation clock by `dt_ms`, then refresh world
/// transforms. Lets a scriptable driver tick a scene whose clips live only in the
/// renderer with no editor transport — notably a `LoadPlayerBundle` runtime reload
/// (loaded via `populate_awsm_scene`, the player path). This is the exact call a game
/// makes each frame, so ticking + screenshotting verifies the player-path skinned
/// animation end-to-end. Async (acquires the renderer lock). `"ok"` or `"error: …"`.
#[wasm_bindgen]
pub async fn editor_tick_animation(dt_ms: f64) -> String {
    let handle = crate::engine::context::renderer_handle();
    let mut r = handle.lock().await;
    if let Err(e) = r.update_animations(dt_ms) {
        return format!("error: {e}");
    }
    r.update_transforms();
    "ok".to_string()
}
