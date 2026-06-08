//! Remote-control link: the editor dials *out* to the native MCP server over
//! WebTransport (QUIC) and serves its requests by calling the
//! [`EditorController`](crate::controller) directly.
//!
//! Started two ways: automatically when the page is loaded with
//! `?mcp=<control-origin>` (e.g. `?mcp=http://127.0.0.1:9086`), or on demand via
//! the top-bar MCP button → connect modal (pre-filled with [`default_origin`], or
//! the `?mcp=` origin if one was supplied, and editable there). Connect /
//! disconnect surface as toasts and a reactive [`status`] signal the UI reflects.
//!
//! Flow: fetch `<control-origin>/control` → `{ quic_url, cert_hash }` → open a
//! WebTransport session pinning that self-signed cert hash → loop accepting
//! server-initiated bidirectional streams, one [`Request`] each, replying with a
//! [`Response`] on the same stream (framing by stream-finish).

use std::cell::RefCell;

use awsm_web_shared::prelude::{Mutable, Toast};
use base64::Engine;
use serde::Deserialize;
use wasm_bindgen_futures::spawn_local;
use web_transport::{ClientBuilder, RecvStream, SendStream, Session};

use awsm_editor_protocol::{Request, Response};

use crate::controller::controller;

/// Cap on a single inbound request (bounds memory if a peer streams without
/// finishing). Requests are small; 16 MiB is far outside the legitimate range.
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// The MCP server's control origin the connect modal pre-fills when no `?mcp=`
/// param was supplied. Baked from `MCP_DEFAULT_ORIGIN` at build time (sourced from
/// `taskfiles/config.yml` → `URL_MCP_DEFAULT`, derived from `PORT_MCP_HTTP_DEV`),
/// falling back to the loopback dev default. The server is always local, so this
/// is the same in dev and prod.
pub fn default_origin() -> &'static str {
    option_env!("MCP_DEFAULT_ORIGIN").unwrap_or("http://127.0.0.1:9086")
}

/// The link's connection state. The top-bar button + modal reflect this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteStatus {
    Disconnected,
    Connecting,
    Connected,
}

thread_local! {
    static STATUS: Mutable<RemoteStatus> = Mutable::new(RemoteStatus::Disconnected);
    static ORIGIN: Mutable<String> = Mutable::new(default_origin().to_string());
    /// The live session, kept so the UI can `disconnect()` it.
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Reactive connection status (for the UI button).
pub fn status() -> Mutable<RemoteStatus> {
    STATUS.with(|s| s.clone())
}

/// The control origin the modal pre-fills (defaults to [`default_origin`];
/// overwritten by `?mcp=` or the last connect attempt).
pub fn origin() -> Mutable<String> {
    ORIGIN.with(|s| s.clone())
}

#[derive(Deserialize)]
struct ControlInfo {
    quic_url: String,
    cert_hash: String,
}

/// Connect to the MCP server at `control_origin`. No-op if already connecting or
/// connected. Surfaces connect / disconnect / failure as toasts and drives the
/// [`status`] signal.
pub fn connect(control_origin: String) {
    let status = status();
    if status.get() != RemoteStatus::Disconnected {
        return; // already connecting or connected
    }
    origin().set(control_origin.clone());
    status.set(RemoteStatus::Connecting);

    spawn_local(async move {
        let result = run(control_origin).await;
        SESSION.with(|s| *s.borrow_mut() = None);
        let was_connected = status.get() == RemoteStatus::Connected;
        status.set(RemoteStatus::Disconnected);
        match (was_connected, result) {
            // Dropped after a successful connect (server stopped, or user clicked
            // disconnect) — informational, not an error.
            (true, res) => {
                if let Err(e) = res {
                    tracing::warn!("mcp link ended: {e}");
                }
                Toast::info("MCP disconnected");
            }
            // Never got connected — the connect itself failed (server down, bad
            // cert, …).
            (false, Err(e)) => Toast::error(format!("MCP connect failed: {e}")),
            (false, Ok(())) => {} // run() only returns Ok via the accept loop ending
        }
    });
}

/// Disconnect the live link (closes the WebTransport session). No-op when not
/// connected. The "MCP disconnected" toast is emitted by the connect task once
/// the accept loop unwinds.
pub fn disconnect() {
    SESSION.with(|s| {
        if let Some(session) = s.borrow().as_ref() {
            session.close(0, "client disconnect");
        }
    });
}

async fn run(control_origin: String) -> Result<(), String> {
    let control_url = format!("{}/control", control_origin.trim_end_matches('/'));
    tracing::info!("mcp: fetching control info from {control_url}");

    let info: ControlInfo = gloo_net::http::Request::get(&control_url)
        .send()
        .await
        .map_err(|e| format!("control fetch: {e}"))?
        .json()
        .await
        .map_err(|e| format!("control decode: {e}"))?;

    let cert_hash = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(info.cert_hash.as_bytes())
        .map_err(|e| format!("bad cert hash: {e}"))?;

    let client = ClientBuilder::new()
        .with_server_certificate_hashes(vec![cert_hash])
        .map_err(|e| format!("client builder: {e}"))?;

    let url: url::Url = info
        .quic_url
        .parse()
        .map_err(|e| format!("bad quic url {}: {e}", info.quic_url))?;

    tracing::info!("mcp: connecting to {url}");
    let session = client
        .connect(url)
        .await
        .map_err(|e| format!("connect: {e}"))?;

    SESSION.with(|s| *s.borrow_mut() = Some(session.clone()));
    status().set(RemoteStatus::Connected);
    Toast::info("MCP connected");
    tracing::info!("mcp: attached");

    loop {
        let (send, recv) = session
            .clone()
            .accept_bi()
            .await
            .map_err(|e| format!("accept_bi: {e}"))?;
        spawn_local(serve_one(send, recv));
    }
}

/// Read one request off a stream, dispatch it, and write the response back.
async fn serve_one(mut send: SendStream, mut recv: RecvStream) {
    let resp = match read_request(&mut recv).await {
        Ok(req) => dispatch(req).await,
        Err(e) => Response::Err(e),
    };
    if let Err(e) = reply(&mut send, &resp).await {
        tracing::warn!("mcp: reply failed: {e}");
    }
}

async fn read_request(recv: &mut RecvStream) -> Result<Request, String> {
    let mut buf = Vec::new();
    while let Some(chunk) = recv
        .read(64 * 1024)
        .await
        .map_err(|e| format!("read: {e}"))?
    {
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_REQUEST_BYTES {
            return Err(format!("request exceeded {MAX_REQUEST_BYTES} bytes"));
        }
    }
    serde_json::from_slice(&buf).map_err(|e| format!("decode request: {e}"))
}

async fn reply(send: &mut SendStream, resp: &Response) -> Result<(), String> {
    let bytes = serde_json::to_vec(resp).map_err(|e| format!("encode response: {e}"))?;
    let mut buf = bytes.as_slice();
    while !buf.is_empty() {
        let n = send.write(buf).await.map_err(|e| format!("write: {e}"))?;
        buf = &buf[n..];
    }
    send.finish().map_err(|e| format!("finish: {e}"))?;
    Ok(())
}

/// Interpret a request against the live controller. All editor mutation flows
/// through `EditorController` (the "all via controller" rule).
async fn dispatch(req: Request) -> Response {
    let ctrl = controller();
    match req {
        Request::Mode => Response::Mode(ctrl.mode.get()),
        Request::Dispatch(cmd) => match ctrl.dispatch(cmd).await {
            Ok(()) => Response::Ok,
            Err(e) => Response::Err(format!("{e}")),
        },
        Request::Query(q) => Response::Query(Box::new(ctrl.query(q).await)),
        Request::Undo => {
            ctrl.undo().await;
            Response::Ok
        }
        Request::Redo => {
            ctrl.redo().await;
            Response::Ok
        }
        Request::ScenePng => png_response(crate::engine::query::scene_png()),
        Request::MaterialPng => png_response(crate::engine::query::material_png()),
        Request::TexturePng(id) => match crate::engine::query::texture_png(id).await {
            Ok(data_url) => png_from_data_url(&data_url),
            Err(e) => Response::Err(e),
        },
    }
}

fn png_response(opt: Option<String>) -> Response {
    match opt {
        Some(data_url) if !data_url.is_empty() => png_from_data_url(&data_url),
        _ => Response::Err("no image available".to_string()),
    }
}

/// Decode a `data:image/png;base64,…` URL into raw PNG bytes.
fn png_from_data_url(data_url: &str) -> Response {
    match data_url.split_once(',') {
        Some((_, b64)) => match base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
            Ok(bytes) => Response::Png(bytes),
            Err(e) => Response::Err(format!("png base64 decode: {e}")),
        },
        None => Response::Err("malformed png data url".to_string()),
    }
}
