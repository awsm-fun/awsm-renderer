//! The HTTP surface:
//! - `GET  /editor`      — the editor's WebSocket upgrade (the live editor link).
//! - `POST /debug`       — raw request seam: a JSON [`Request`] body is relayed to
//!   the attached editor and its [`Response`] returned as JSON. The dev test seam.
//! - `POST /png/{id}` / `GET /png/{id}` — the PNG byte side-channel: the editor
//!   POSTs rendered image bytes here (off the control link); the rmcp tool layer
//!   (and humans/tooling) read them back.
//! - `GET  /health`      — agent-facing liveness (editor attached? last boot error?).
//! - `POST /boot-error`  — the editor reports a renderer/init failure (before any
//!   MCP attach), so a boot crash is visible to agents via `/health`.
//! - `/mcp`              — the rmcp streamable-HTTP endpoint mounts onto this router.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::DefaultBodyLimit;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::{extract::Path, extract::State, routing::get, routing::post, Json, Router};
use serde_json::{json, Value};
use tower_http::cors::{Any, CorsLayer};

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use awsm_renderer_editor_protocol::Request;

use crate::link::EditorLink;
use crate::mcp::EditorMcp;

/// Cap on retained PNG files (bounds temp-dir disk use). Renders past this are
/// evicted oldest-first and their files deleted.
const MAX_RETAINED_PNGS: usize = 32;

/// Body-size cap for a PNG upload. A high-res scene render is a few MiB, well past
/// Axum's 2 MB default — which would silently 413 a non-trivial screenshot. This
/// is a loopback-only side-channel from the trusted local editor; the cap exists
/// only to bound memory (the body is buffered before the temp-file write).
const PNG_BODY_LIMIT: usize = 256 * 1024 * 1024;

/// On-disk path the editor's PNG upload lands at (and the rmcp tool reads back).
/// Both sides agree on this naming so the tool needs no shared in-memory map.
pub(crate) fn png_path(id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("awsm-renderer-scene-mcp-{id}.png"))
}

#[derive(Clone)]
struct AppState {
    link: EditorLink,
    /// Insertion-ordered PNG ids for LRU eviction (see [`MAX_RETAINED_PNGS`]).
    pngs: Arc<Mutex<VecDeque<String>>>,
    /// The most recent editor BOOT error (renderer/init failure reported by the
    /// page before any MCP attach happened), with a timestamp. Agents read it via
    /// `GET /health` — without this, a boot-time failure is invisible outside the
    /// browser console (the editor never attaches, so every request just errors
    /// with "no editor attached" and no cause).
    boot_error: Arc<Mutex<Option<(std::time::SystemTime, String)>>>,
}

/// Serve the HTTP surface on `addr` until shutdown.
pub async fn serve(addr: SocketAddr, link: EditorLink) -> Result<()> {
    let state = AppState {
        link: link.clone(),
        pngs: Arc::new(Mutex::new(VecDeque::new())),
        boot_error: Arc::new(Mutex::new(None)),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        // Private Network Access: let a public HTTPS page (e.g. the hosted editor)
        // reach this loopback server -- Chrome demands this opt-in on the preflight.
        .allow_private_network(true);

    // The rmcp MCP endpoint: a streamable-HTTP tower service mounted at /mcp.
    // A fresh handler is built per session, sharing the (Arc-backed) editor link.
    //
    // Long-lived sessions: rmcp's default drops a session after 5 min idle — far
    // too short for an interactive coding agent that sits idle between tool calls.
    // That idle "safety net" is for servers behind proxies that silently drop
    // connections; we're loopback-only, so use a day-long timeout (still reclaims
    // a genuinely-dead session, but never an idle-but-live one).
    let mut session_manager = LocalSessionManager::default();
    session_manager.session_config.keep_alive = Some(Duration::from_secs(60 * 60 * 24));
    let mcp_link = link.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(EditorMcp::new(mcp_link.clone())),
        Arc::new(session_manager),
        StreamableHttpServerConfig::default(),
    );

    let app = Router::new()
        // The editor dials out to this WebSocket for the live link.
        .route("/editor", get(editor_ws))
        .route("/debug", post(debug))
        .route("/boot-error", post(boot_error))
        .route("/health", get(health))
        // The PNG side-channel: the editor POSTs rendered image bytes here (off
        // the control link); the rmcp tool / humans GET them back. Raise the body
        // cap well past Axum's 2 MB default so multi-MiB renders aren't rejected.
        .route(
            "/png/{id}",
            post(png_upload)
                .get(png_download)
                .layer(DefaultBodyLimit::max(PNG_BODY_LIMIT)),
        )
        .nest_service("/mcp", mcp_service)
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("http listening on http://{addr} (/mcp, /editor, /debug, /png, /health)");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Upgrade the editor's `/editor` request to a WebSocket and hand it to the link.
async fn editor_ws(ws: WebSocketUpgrade, State(s): State<AppState>) -> impl IntoResponse {
    let link = s.link.clone();
    ws.on_upgrade(move |socket| crate::ws::handle_socket(socket, link))
}

/// Relay a raw [`Request`] (JSON body) to the editor and return its [`Response`]
/// as JSON. A PNG request returns the `PngHandle` JSON; the bytes are at
/// `/png/<id>`.
async fn debug(State(s): State<AppState>, Json(req): Json<Request>) -> Json<Value> {
    match s.link.debug_request(&req).await {
        Ok(resp) => Json(
            serde_json::to_value(&resp)
                .unwrap_or_else(|e| json!({ "encode_error": e.to_string() })),
        ),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// The editor page reports a BOOT failure (renderer init error) here —
/// fire-and-forget from the browser, before/without any MCP attach.
async fn boot_error(State(s): State<AppState>, body: String) -> Json<Value> {
    tracing::error!("editor boot error reported: {body}");
    *s.boot_error.lock().unwrap() = Some((std::time::SystemTime::now(), body));
    Json(json!({ "ok": true }))
}

/// Agent-facing liveness: is an editor attached, and did the last page load
/// report a boot error? Check THIS when requests go unanswered.
async fn health(State(s): State<AppState>) -> Json<Value> {
    let attached = s.link.connection_count() > 0;
    let boot = s.boot_error.lock().unwrap().clone().map(|(t, msg)| {
        let age = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        json!({ "age_seconds": age, "message": msg })
    });
    Json(json!({ "editor_attached": attached, "last_boot_error": boot }))
}

/// `POST /png/{id}` — the editor uploads a rendered PNG here (off the control
/// link). We write it to a temp file and remember the id for LRU eviction.
async fn png_upload(State(s): State<AppState>, Path(id): Path<String>, body: Bytes) -> StatusCode {
    let path = png_path(&id);
    if let Err(e) = std::fs::write(&path, &body) {
        tracing::warn!("png upload write failed ({}): {e}", path.display());
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    tracing::debug!("png {id}: {} bytes → {}", body.len(), path.display());
    // Track for eviction; drop the oldest beyond the cap.
    let mut q = s.pngs.lock().unwrap();
    q.push_back(id);
    while q.len() > MAX_RETAINED_PNGS {
        if let Some(old) = q.pop_front() {
            let _ = std::fs::remove_file(png_path(&old));
        }
    }
    StatusCode::OK
}

/// `GET /png/{id}` — serve a previously-uploaded render (for humans / tooling).
async fn png_download(Path(id): Path<String>) -> impl IntoResponse {
    let id = id.strip_suffix(".png").unwrap_or(&id);
    match std::fs::read(png_path(id)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such png").into_response(),
    }
}
