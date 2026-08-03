//! The HTTP surface:
//! - `GET  /editor`      — the editor's WebSocket upgrade (the live editor link).
//! - `POST /debug`       — raw request seam: a JSON [`Request`] body is relayed to
//!   the attached editor and its [`Response`] returned as JSON. The dev test seam.
//! - `POST /png/{id}` / `GET /png/{id}` — the PNG byte side-channel: the editor
//!   POSTs rendered image bytes here (off the control link); the rmcp tool layer
//!   (and humans/tooling) read them back.
//! - `POST /glb/{id}` / `GET /glb/{id}` — the same side-channel for exported `.glb`
//!   bytes (`export_scene_glb` / `export_node_glb`); the tool layer returns the
//!   temp-file path so the bytes never cross the control link or the token stream.
//! - `POST /bundle/{id}/{*path}` / `GET /bundle/{id}/{*path}` — the same
//!   side-channel for player-bundle files (`export_player_bundle`): the editor
//!   POSTs each baked file under its bundle-relative path; they land in one temp
//!   directory per id, and the tool layer returns that directory's path.
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

/// Cap on retained `.glb` files (bounds temp-dir disk use). Exports past this are
/// evicted oldest-first and their files deleted.
const MAX_RETAINED_GLBS: usize = 16;

/// Body-size cap for a PNG upload. A high-res scene render is a few MiB, well past
/// Axum's 2 MB default — which would silently 413 a non-trivial screenshot. This
/// is a loopback-only side-channel from the trusted local editor; the cap exists
/// only to bound memory (the body is buffered before the temp-file write).
const PNG_BODY_LIMIT: usize = 256 * 1024 * 1024;

/// Body-size cap for a `.glb` upload — a whole-scene export can be large, so use
/// the same generous loopback-only cap as PNG (see [`PNG_BODY_LIMIT`]).
const GLB_BODY_LIMIT: usize = 256 * 1024 * 1024;

/// Cap on retained player-bundle directories (bounds temp-dir disk use). Exports
/// past this are evicted oldest-first and their directories deleted.
const MAX_RETAINED_BUNDLES: usize = 8;

/// Body-size cap for a single bundle-file upload (per file; a bundle is many
/// POSTs). Same generous loopback-only rationale as [`PNG_BODY_LIMIT`].
const BUNDLE_BODY_LIMIT: usize = 256 * 1024 * 1024;

/// On-disk path the editor's PNG upload lands at (and the rmcp tool reads back).
/// Both sides agree on this naming so the tool needs no shared in-memory map.
pub(crate) fn png_path(id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("awsm-renderer-scene-mcp-{id}.png"))
}

/// On-disk path the editor's `.glb` export upload lands at. The rmcp tool returns
/// this path verbatim (it does not inline the bytes), and `GET /glb/<id>` serves
/// it for humans / tooling.
pub(crate) fn glb_path(id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("awsm-renderer-scene-mcp-{id}.glb"))
}

/// On-disk directory a player-bundle upload lands in (one per export). The rmcp
/// tool returns this path verbatim (it never inlines the bytes), and
/// `GET /bundle/<id>/<path>` serves the files for humans / a player dev server.
pub(crate) fn bundle_dir(id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("awsm-renderer-scene-mcp-bundle-{id}"))
}

/// Resolve a bundle-relative file path under its bundle's temp directory,
/// rejecting anything that could escape it (traversal components, absolute
/// paths, a malformed id). `None` means "don't touch the filesystem".
fn bundle_file_path(id: &str, rel: &str) -> Option<PathBuf> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let mut path = bundle_dir(id);
    for comp in rel.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." || comp.contains('\\') {
            return None;
        }
        path.push(comp);
    }
    Some(path)
}

#[derive(Clone)]
struct AppState {
    link: EditorLink,
    /// Insertion-ordered PNG ids for LRU eviction (see [`MAX_RETAINED_PNGS`]).
    pngs: Arc<Mutex<VecDeque<String>>>,
    /// Insertion-ordered `.glb` ids for LRU eviction (see [`MAX_RETAINED_GLBS`]).
    glbs: Arc<Mutex<VecDeque<String>>>,
    /// Insertion-ordered bundle ids for LRU eviction (see
    /// [`MAX_RETAINED_BUNDLES`]). One entry per bundle (not per file).
    bundles: Arc<Mutex<VecDeque<String>>>,
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
        glbs: Arc::new(Mutex::new(VecDeque::new())),
        bundles: Arc::new(Mutex::new(VecDeque::new())),
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
        // The `.glb` side-channel: the editor POSTs exported glTF bytes here; the
        // rmcp tool returns the temp-file path (never the inline bytes).
        .route(
            "/glb/{id}",
            post(glb_upload)
                .get(glb_download)
                .layer(DefaultBodyLimit::max(GLB_BODY_LIMIT)),
        )
        // The player-bundle side-channel: the editor POSTs each baked file here
        // under its bundle-relative path; the rmcp tool returns the bundle
        // directory path (never the inline bytes), and GET serves the files.
        .route(
            "/bundle/{id}/{*path}",
            post(bundle_upload)
                .get(bundle_download)
                .layer(DefaultBodyLimit::max(BUNDLE_BODY_LIMIT)),
        )
        .nest_service("/mcp", mcp_service)
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        "http listening on http://{addr} (/mcp, /editor, /debug, /png, /glb, /bundle, /health)"
    );
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

/// `POST /glb/{id}` — the editor uploads an exported `.glb` here (off the control
/// link). We write it to a temp file and remember the id for LRU eviction.
async fn glb_upload(State(s): State<AppState>, Path(id): Path<String>, body: Bytes) -> StatusCode {
    // Symmetric with `glb_download`, which strips a trailing `.glb` so a model
    // can be GET as `/glb/<id>.glb` (a real-looking model URL) while stored under
    // `<id>`. Strip here too so POST `/glb/<id>.glb` and POST `/glb/<id>`
    // canonicalize to the SAME file — otherwise the `.glb`-suffixed POST writes
    // `<id>.glb.glb`, which the suffix-stripping GET can never read, yielding a
    // silent 404 on an upload that returned 200.
    let id = canonical_glb_id(&id).to_string();
    let path = glb_path(&id);
    if let Err(e) = std::fs::write(&path, &body) {
        tracing::warn!("glb upload write failed ({}): {e}", path.display());
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    tracing::debug!("glb {id}: {} bytes → {}", body.len(), path.display());
    // Track for eviction; drop the oldest beyond the cap.
    let mut q = s.glbs.lock().unwrap();
    q.push_back(id);
    while q.len() > MAX_RETAINED_GLBS {
        if let Some(old) = q.pop_front() {
            let _ = std::fs::remove_file(glb_path(&old));
        }
    }
    StatusCode::OK
}

/// Canonical stored id for `/glb/{id}`: a trailing `.glb` is stripped so a
/// model can be addressed as a real-looking `/glb/<id>.glb` URL while stored
/// under `<id>`. Upload and download MUST both go through this — an
/// asymmetric strip once produced 200-OK uploads (`<id>.glb.glb`) that every
/// subsequent GET 404'd.
fn canonical_glb_id(id: &str) -> &str {
    id.strip_suffix(".glb").unwrap_or(id)
}

/// `GET /glb/{id}` — serve a previously-exported `.glb` (for humans / tooling).
async fn glb_download(Path(id): Path<String>) -> impl IntoResponse {
    let id = canonical_glb_id(&id);
    match std::fs::read(glb_path(id)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "model/gltf-binary")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such glb").into_response(),
    }
}

/// On-disk content-addressed cache of previously-uploaded bundle-file bytes,
/// keyed by their sha256 hex. Lets a repeat export skip re-uploading unchanged
/// media: the editor first POSTs an EMPTY body with `?sha256=<hex>`; a cache
/// hit copies server-side (fast, loopback disk), a miss answers 412 and the
/// editor re-POSTs the real bytes (which then populate the cache).
fn upload_cache_path(sha256: &str) -> Option<PathBuf> {
    // Strict hex validation — the hash becomes a filename.
    if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let dir = std::env::temp_dir().join("awsm-renderer-scene-mcp-upload-cache");
    Some(dir.join(sha256.to_ascii_lowercase()))
}

#[derive(serde::Deserialize, Default)]
struct BundleUploadQuery {
    /// sha256 hex of the file's bytes (see [`upload_cache_path`]).
    sha256: Option<String>,
}

/// `POST /bundle/{id}/{*path}` — the editor uploads one baked bundle file here
/// (off the control link), under its bundle-relative path. We write it into the
/// bundle's temp directory and remember the id for LRU eviction.
///
/// With `?sha256=<hex>`: an EMPTY body is a cache probe — 200 when the bytes
/// were copied from the upload cache, 412 (Precondition Failed) when absent
/// (re-POST with the body); a non-empty body is written normally AND recorded
/// in the cache for future probes.
async fn bundle_upload(
    State(s): State<AppState>,
    Path((id, rel)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<BundleUploadQuery>,
    body: Bytes,
) -> StatusCode {
    let Some(path) = bundle_file_path(&id, &rel) else {
        tracing::warn!("bundle upload rejected: bad id/path ({id} / {rel})");
        return StatusCode::BAD_REQUEST;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("bundle upload mkdir failed ({}): {e}", parent.display());
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }
    let cache = q.sha256.as_deref().and_then(upload_cache_path);
    if body.is_empty() && q.sha256.is_some() {
        // Cache probe. (A legitimately empty file also lands here; copying a
        // cached empty file and writing an empty one are equivalent, and a
        // miss falls through to the re-POST which writes it plainly.)
        let Some(cache) = cache else {
            tracing::warn!("bundle upload: malformed sha256 for {rel}");
            return StatusCode::BAD_REQUEST;
        };
        return match std::fs::copy(&cache, &path) {
            Ok(n) => {
                tracing::debug!("bundle {id}: {rel} ← upload cache ({n} bytes)");
                track_bundle_for_eviction(&s, id);
                StatusCode::OK
            }
            Err(_) => StatusCode::PRECONDITION_FAILED,
        };
    }
    if let Err(e) = std::fs::write(&path, &body) {
        tracing::warn!("bundle upload write failed ({}): {e}", path.display());
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    // Populate the content cache (best-effort; never fails the upload). Trust
    // the editor's hash — it's the same trusted loopback peer that sent the
    // bytes, and a wrong hash only poisons its own future skip.
    if let Some(cache) = cache {
        if let Some(dir) = cache.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&cache, &body);
    }
    tracing::debug!("bundle {id}: {} bytes → {}", body.len(), path.display());
    track_bundle_for_eviction(&s, id);
    StatusCode::OK
}

/// Track a bundle id for LRU eviction on its FIRST file; drop the oldest
/// beyond the cap (whole directories — a bundle is one unit).
fn track_bundle_for_eviction(s: &AppState, id: String) {
    let mut q = s.bundles.lock().unwrap();
    if !q.contains(&id) {
        q.push_back(id);
        while q.len() > MAX_RETAINED_BUNDLES {
            if let Some(old) = q.pop_front() {
                let _ = std::fs::remove_dir_all(bundle_dir(&old));
            }
        }
    }
}

/// `GET /bundle/{id}/{*path}` — serve a previously-uploaded bundle file (for
/// humans / a player dev server pointed at the bundle over HTTP).
async fn bundle_download(Path((id, rel)): Path<(String, String)>) -> impl IntoResponse {
    let Some(path) = bundle_file_path(&id, &rel) else {
        return (StatusCode::BAD_REQUEST, "bad bundle path").into_response();
    };
    match std::fs::read(&path) {
        Ok(bytes) => ([(header::CONTENT_TYPE, bundle_mime(&rel))], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such bundle file").into_response(),
    }
}

/// Content type for a served bundle file, by extension (enough for a browser /
/// player to consume the handful of formats a bundle contains).
fn bundle_mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("toml") | Some("wgsl") | Some("txt") => "text/plain; charset=utf-8",
        Some("glb") => "model/gltf-binary",
        Some("png") => "image/png",
        Some("ktx2") => "image/ktx2",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::canonical_glb_id;

    /// POST `/glb/<id>.glb` and GET `/glb/<id>.glb` must address the same
    /// stored id as their suffix-free forms (the upload/download asymmetry
    /// regression from MCP-BUGS 2026-07-08).
    #[test]
    fn glb_id_suffix_symmetry() {
        assert_eq!(canonical_glb_id("model.glb"), "model");
        assert_eq!(canonical_glb_id("model"), "model");
        assert_eq!(canonical_glb_id("model.glb.glb"), "model.glb");
        assert_eq!(canonical_glb_id("a1b2-c3d4"), "a1b2-c3d4");
    }
}
