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
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

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
