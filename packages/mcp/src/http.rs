//! The HTTP control surface:
//! - `GET  /control` — CORS-open; the editor fetches the QUIC URL + cert hash to
//!   pin before opening its WebTransport session.
//! - `POST /debug`   — raw request seam: a JSON [`Request`] body is relayed to
//!   the attached editor and its [`Response`] returned as JSON (PNGs are written
//!   to a temp file and summarized). The pre-rmcp test-client entry point.
//!
//! The rmcp `/mcp` endpoint mounts onto this same router in a later phase.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::{extract::State, routing::get, routing::post, Json, Router};
use serde_json::{json, Value};
use tower_http::cors::{Any, CorsLayer};

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use awsm_editor_protocol::{Request, Response};

use crate::cert::GeneratedCert;
use crate::link::EditorLink;
use crate::mcp::EditorMcp;

#[derive(Clone)]
struct AppState {
    cert_hash: String,
    quic_port: u16,
    link: EditorLink,
    /// The most recent editor BOOT error (renderer/init failure reported by
    /// the page before any MCP attach happened), with a timestamp. Agents
    /// read it via `GET /health` — without this, a boot-time failure is
    /// invisible outside the browser console (the editor never attaches, so
    /// every /debug request just times out with no cause).
    boot_error: Arc<std::sync::Mutex<Option<(std::time::SystemTime, String)>>>,
}

/// Serve the control HTTP surface on `addr` until shutdown.
pub async fn serve(
    addr: SocketAddr,
    cert: Arc<GeneratedCert>,
    quic_port: u16,
    link: EditorLink,
) -> Result<()> {
    let state = AppState {
        cert_hash: cert.hash_base64url(),
        quic_port,
        link: link.clone(),
        boot_error: Arc::new(std::sync::Mutex::new(None)),
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
    let mcp_link = link.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(EditorMcp::new(mcp_link.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );

    let app = Router::new()
        .route("/control", get(control))
        .route("/debug", post(debug))
        .route("/boot-error", post(boot_error))
        .route("/health", get(health))
        .nest_service("/mcp", mcp_service)
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("control http listening on http://{addr}/control");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn control(State(s): State<AppState>) -> Json<Value> {
    Json(json!({
        "quic_url": format!("https://127.0.0.1:{}", s.quic_port),
        "cert_hash": s.cert_hash,
    }))
}

/// The editor page reports a BOOT failure (renderer init error) here —
/// fire-and-forget from the browser, before/without any MCP attach.
async fn boot_error(State(s): State<AppState>, body: String) -> Json<Value> {
    tracing::error!("editor boot error reported: {body}");
    *s.boot_error.lock().unwrap() = Some((std::time::SystemTime::now(), body));
    Json(json!({ "ok": true }))
}

/// Agent-facing liveness: is an editor attached, and did the last page load
/// report a boot error? Check THIS when /debug requests go unanswered.
async fn health(State(s): State<AppState>) -> Json<Value> {
    let attached = s.link.session().await.is_some();
    let boot = s.boot_error.lock().unwrap().clone().map(|(t, msg)| {
        let age = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        json!({ "age_seconds": age, "message": msg })
    });
    Json(json!({ "editor_attached": attached, "last_boot_error": boot }))
}

/// Relay a raw [`Request`] (JSON body) to the editor and return its [`Response`].
async fn debug(State(s): State<AppState>, Json(req): Json<Request>) -> Json<Value> {
    match s.link.request(&req).await {
        Ok(Response::Png(bytes)) => {
            let path = std::env::temp_dir().join("awsm-mcp-last.png");
            let saved = std::fs::write(&path, &bytes).is_ok();
            Json(json!({
                "Png": { "bytes": bytes.len(), "saved": saved, "path": path.to_string_lossy() }
            }))
        }
        Ok(resp) => Json(
            serde_json::to_value(&resp)
                .unwrap_or_else(|e| json!({ "encode_error": e.to_string() })),
        ),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}
