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

use std::cell::{Cell, RefCell};

use awsm_web_shared::prelude::{Mutable, Toast};
use base64::Engine;
use serde::Deserialize;
use wasm_bindgen_futures::spawn_local;
use web_transport::{ClientBuilder, RecvStream, SendStream, Session};

use awsm_editor_protocol::{EditorEvent, Request, Response};

use crate::controller::controller;

/// Cap on a single inbound request (bounds memory if a peer streams without
/// finishing). Requests are small; 16 MiB is far outside the legitimate range.
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// How long the "agent working" pulse lingers after the last in-flight request
/// finishes, so a burst of quick mutations doesn't flicker the indicator on/off.
const WORKING_COOLDOWN_MS: u32 = 450;

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
    /// True while the MCP agent is actively serving requests (drives the UI pulse).
    static WORKING: Mutable<bool> = Mutable::new(false);
    /// Count of in-flight requests (a long render keeps the pulse lit).
    static IN_FLIGHT: Cell<u32> = const { Cell::new(0) };
    /// Bumped whenever activity starts/stops; lets a queued cooldown cancel itself.
    static IDLE_GEN: Cell<u64> = const { Cell::new(0) };
    /// True while a connect/reconnect task is active — the editor keeps re-dialing
    /// the MCP server (with backoff) until [`disconnect`] clears it. Lets a server
    /// restart reconnect seamlessly without a manual page reload.
    static RECONNECT: Cell<bool> = const { Cell::new(false) };
}

/// Reactive connection status (for the UI button).
pub fn status() -> Mutable<RemoteStatus> {
    STATUS.with(|s| s.clone())
}

/// Reactive "agent working" flag — true while the MCP agent is serving requests
/// (plus a short cooldown). The top-bar MCP chip pulses on this so the human
/// knows the agent is mid-edit and changes are landing live. Informational only:
/// the editor stays fully interactive (every edit is command-sourced + undoable),
/// matching the awsm-audio convention — no hard lock on human input.
pub fn working() -> Mutable<bool> {
    WORKING.with(|w| w.clone())
}

/// Mark the start of serving one MCP request: light the pulse, cancel any pending
/// idle cooldown.
fn activity_begin() {
    IN_FLIGHT.with(|c| c.set(c.get() + 1));
    IDLE_GEN.with(|g| g.set(g.get().wrapping_add(1)));
    WORKING.with(|w| w.set_neq(true));
}

/// Mark one request done. When the last one finishes, keep the pulse lit for a
/// short cooldown, then clear it if still idle (so bursts don't flicker).
async fn activity_end() {
    let remaining = IN_FLIGHT.with(|c| {
        let n = c.get().saturating_sub(1);
        c.set(n);
        n
    });
    if remaining != 0 {
        return;
    }
    let generation = IDLE_GEN.with(|g| {
        let n = g.get().wrapping_add(1);
        g.set(n);
        n
    });
    gloo_timers::future::TimeoutFuture::new(WORKING_COOLDOWN_MS).await;
    // Still idle and no newer activity since we queued? Then we're truly done.
    if IDLE_GEN.with(|g| g.get()) == generation && IN_FLIGHT.with(|c| c.get()) == 0 {
        WORKING.with(|w| w.set_neq(false));
    }
}

/// Force the pulse off (on disconnect) so a stale "working" never lingers.
fn activity_reset() {
    IN_FLIGHT.with(|c| c.set(0));
    IDLE_GEN.with(|g| g.set(g.get().wrapping_add(1)));
    WORKING.with(|w| w.set_neq(false));
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

/// Connect to the MCP server at `control_origin`, **staying connected**: if the
/// link drops (e.g. the server restarts), re-dial with backoff until
/// [`disconnect`] is called. No-op if a connect/reconnect task is already active.
/// Surfaces connect / disconnect / failure as toasts and drives the [`status`]
/// signal. The seamless reconnect is what lets the dev MCP server restart (new
/// commands/tools) without a manual editor reload.
pub fn connect(control_origin: String) {
    if RECONNECT.with(|r| r.get()) {
        return; // a connect/reconnect task is already running
    }
    origin().set(control_origin.clone());
    RECONNECT.with(|r| r.set(true));
    status().set(RemoteStatus::Connecting);

    spawn_local(async move {
        let mut backoff_ms = 500u32;
        let mut toasted_failure = false;
        loop {
            let result = run(control_origin.clone()).await;
            SESSION.with(|s| *s.borrow_mut() = None);
            activity_reset();
            let was_connected = status().get() == RemoteStatus::Connected;
            status().set(RemoteStatus::Disconnected);

            // Explicit disconnect (RECONNECT cleared) — stop, no retry.
            if !RECONNECT.with(|r| r.get()) {
                if was_connected {
                    Toast::info("MCP disconnected");
                }
                break;
            }

            // Otherwise keep the link alive by re-dialing.
            if was_connected {
                Toast::info("MCP disconnected \u{2014} reconnecting\u{2026}");
                backoff_ms = 500; // a live link dropped; retry promptly
                toasted_failure = false;
            } else if let Err(e) = &result {
                // Connect attempt failed (server not up yet / bad cert). Toast
                // once, then retry quietly so we don't spam while it comes up.
                if !toasted_failure {
                    Toast::error(format!("MCP connect failed (retrying): {e}"));
                    toasted_failure = true;
                }
                tracing::warn!("mcp connect failed (retrying): {e}");
            }

            gloo_timers::future::TimeoutFuture::new(backoff_ms).await;
            if !RECONNECT.with(|r| r.get()) {
                break; // disconnected during the backoff
            }
            backoff_ms = (backoff_ms.saturating_mul(2)).min(3000);
            status().set(RemoteStatus::Connecting);
        }
        RECONNECT.with(|r| r.set(false)); // task done; allow a future connect()
    });
}

/// Push an editor → agent event over the link (compile/runtime toast, selection
/// change). No-op when no link is attached. Each event rides its own
/// unidirectional stream (framed by stream-finish); the MCP server relays it to
/// the agent as a logging notification. Best-effort — failures are logged, never
/// surfaced (the editor must not block on the agent being connected).
pub fn notify_event(event: EditorEvent) {
    let session = SESSION.with(|s| s.borrow().clone());
    let Some(session) = session else {
        return;
    };
    spawn_local(async move {
        if let Err(e) = send_event(session, &event).await {
            tracing::debug!("mcp notify failed: {e}");
        }
    });
}

async fn send_event(session: Session, ev: &EditorEvent) -> Result<(), String> {
    let mut send = session
        .open_uni()
        .await
        .map_err(|e| format!("open_uni: {e}"))?;
    let bytes = serde_json::to_vec(ev).map_err(|e| format!("encode event: {e}"))?;
    let mut buf = bytes.as_slice();
    while !buf.is_empty() {
        let n = send.write(buf).await.map_err(|e| format!("write: {e}"))?;
        buf = &buf[n..];
    }
    send.finish().map_err(|e| format!("finish: {e}"))?;
    Ok(())
}

/// Disconnect the live link and **stop reconnecting** (clears the reconnect
/// flag, then closes the WebTransport session). The connect task sees the flag
/// cleared and exits instead of re-dialing. The "MCP disconnected" toast is
/// emitted by that task as it unwinds.
pub fn disconnect() {
    RECONNECT.with(|r| r.set(false));
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
    activity_begin();
    let resp = match read_request(&mut recv).await {
        Ok(req) => dispatch(req).await,
        Err(e) => Response::Err(e),
    };
    if let Err(e) = reply(&mut send, &resp).await {
        tracing::warn!("mcp: reply failed: {e}");
    }
    activity_end().await;
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
        Request::DispatchBatch(cmds) => match ctrl.dispatch_batch(cmds).await {
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
        Request::ScenePng { width, height } => {
            png_response(crate::engine::query::scene_png(width, height).await)
        }
        Request::MaterialPng { width, height } => {
            png_response(crate::engine::query::material_png(width, height))
        }
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
