//! Remote-control link: the editor dials *out* to the native MCP server over a
//! WebSocket (`<origin>/editor`) and serves its requests by calling the
//! [`EditorController`](crate::controller) directly.
//!
//! Started two ways: automatically when the page is loaded with
//! `?mcp=<control-origin>` (e.g. `?mcp=http://127.0.0.1:9086`), or on demand via
//! the top-bar MCP button → connect modal (pre-filled with [`default_origin`], or
//! the `?mcp=` origin if one was supplied, and editable there). Connect /
//! disconnect surface as toasts and a reactive [`status`] signal the UI reflects.
//!
//! The link is one ordered WebSocket. The server sends [`WsServerMsg::Request`]
//! frames; we serve each and reply with a [`WsClientMsg::Response`] carrying the
//! same `id`. Editor push events go up as [`WsClientMsg::Event`]. All outbound
//! frames funnel through a single writer (an mpsc drained in [`run`]) so
//! concurrent replies/events never interleave a half-written frame. Rendered PNGs
//! ride a `/png/<id>` HTTP side-channel (the bytes never cross the link); only a
//! small [`PngHandle`] comes back over the socket.

use std::cell::{Cell, RefCell};

use awsm_renderer_web_shared::prelude::{Mutable, Toast};
use base64::Engine;
use futures::channel::mpsc;
use futures::{FutureExt, SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use wasm_bindgen_futures::spawn_local;

use awsm_renderer_editor_protocol::{
    BundleFileMeta, BundleHandle, EditorEvent, GlbHandle, PngHandle, Request, Response,
    WsClientMsg, WsServerMsg,
};

use crate::controller::controller;

/// Outbound-frame sender: every reply / event funnels through this to the single
/// writer in [`run`].
type LinkTx = mpsc::UnboundedSender<WsClientMsg>;

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
    /// Use TLS for the link (`wss`/`https`) instead of plain (`ws`/`http`). Off by
    /// default — the server is normally local. Set via the modal toggle for a
    /// TLS-terminated remote server.
    static TLS: Mutable<bool> = Mutable::new(false);
    /// User setting: when `false`, MCP connect/disconnect/error notices are
    /// suppressed (the link still works; only the toasts are muted). Bound to a
    /// Settings toggle; defaults on. Session-only chrome, not project state.
    // Default OFF: MCP info toasts overlay the viewport and contaminate
    // agent screenshots; opt in via Settings or SetViewOptions.
    static SHOW_NOTIFICATIONS: Mutable<bool> = Mutable::new(false);
    /// Outbound frame sender for the live link; `None` when disconnected. Kept so
    /// the UI can `disconnect()` (drop it) and `notify_event()` over the socket.
    static SESSION: RefCell<Option<LinkTx>> = const { RefCell::new(None) };
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
    /// Whether the `visibilitychange` push listener is installed (once per page —
    /// it outlives individual link sessions; `notify_event` is a no-op when
    /// disconnected).
    static VISIBILITY_HOOKED: Cell<bool> = const { Cell::new(false) };
}

/// Push the tab's current visibility to the server (`kind: "visibility"`). The
/// server records it per connection so frame-bound requests can fail fast with a
/// "tab hidden" error instead of burning the full request timeout, and surfaces
/// it in `ping`/`pairing_status`. Sent on attach and on every `visibilitychange`.
fn push_visibility() {
    let hidden = web_sys::window()
        .and_then(|w| w.document())
        .map(|d| d.hidden())
        .unwrap_or(false);
    notify_event(EditorEvent {
        kind: "visibility".to_string(),
        level: None,
        message: None,
        nodes: None,
        hidden: Some(hidden),
    });
}

/// Install the page-level `visibilitychange` listener (idempotent). Lives for the
/// page: reconnects reuse it, and pushes while disconnected are dropped by
/// [`notify_event`].
fn hook_visibility() {
    if VISIBILITY_HOOKED.with(|h| h.replace(true)) {
        return;
    }
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let cb = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(push_visibility);
    use wasm_bindgen::JsCast;
    let _ = doc.add_event_listener_with_callback("visibilitychange", cb.as_ref().unchecked_ref());
    cb.forget(); // page-lifetime listener
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

/// Whether the link uses TLS (`wss`/`https`). Off by default; the connect modal's
/// toggle flips it for a TLS-terminated remote server.
pub fn tls() -> Mutable<bool> {
    TLS.with(|s| s.clone())
}

/// Reactive "show MCP notifications" setting, for a Settings checkbox. When off,
/// MCP connect/disconnect/error toasts are suppressed (the link still works).
pub fn show_notifications() -> Mutable<bool> {
    SHOW_NOTIFICATIONS.with(|s| s.clone())
}

/// Emit an MCP info toast, gated by the [`show_notifications`] setting.
fn notify_info(msg: impl Into<String>) {
    if SHOW_NOTIFICATIONS.with(|s| s.get()) {
        Toast::info(msg);
    }
}

/// Emit an MCP error toast, gated by the [`show_notifications`] setting.
fn notify_error(msg: impl Into<String>) {
    if SHOW_NOTIFICATIONS.with(|s| s.get()) {
        Toast::error(msg);
    }
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
                    notify_info("MCP disconnected");
                }
                break;
            }

            // Otherwise keep the link alive by re-dialing.
            if was_connected {
                notify_info("MCP disconnected \u{2014} reconnecting\u{2026}");
                backoff_ms = 500; // a live link dropped; retry promptly
                toasted_failure = false;
            } else if let Err(e) = &result {
                // Connect attempt failed (server not up yet). Toast once, then
                // retry quietly so we don't spam while it comes up.
                if !toasted_failure {
                    notify_error(format!("MCP connect failed (retrying): {e}"));
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

/// Disconnect the live link and **stop reconnecting** (clears the reconnect flag,
/// then drops the outbound sender, which ends [`run`]). The connect task sees the
/// flag cleared and exits instead of re-dialing. The "MCP disconnected" toast is
/// emitted by that task as it unwinds.
pub fn disconnect() {
    RECONNECT.with(|r| r.set(false));
    SESSION.with(|s| *s.borrow_mut() = None);
}

/// Push an editor → agent event over the link (compile/runtime toast, selection
/// change). No-op when no link is attached. The MCP server relays it to the
/// connected agent as a logging notification. Best-effort — failures are silent
/// (the editor must not block on the agent being connected).
pub fn notify_event(event: EditorEvent) {
    SESSION.with(|s| {
        if let Some(tx) = s.borrow().as_ref() {
            let _ = tx.unbounded_send(WsClientMsg::Event(event));
        }
    });
}

/// Queue an outbound frame on the live link (best-effort).
fn send_frame(frame: WsClientMsg) {
    SESSION.with(|s| {
        if let Some(tx) = s.borrow().as_ref() {
            let _ = tx.unbounded_send(frame);
        }
    });
}

/// Strip any URL scheme (`http(s)://`, `ws(s)://`) and trailing slash from a
/// control origin, leaving a bare `host:port` authority.
fn authority(origin: &str) -> &str {
    let o = origin.trim().trim_end_matches('/');
    o.strip_prefix("https://")
        .or_else(|| o.strip_prefix("http://"))
        .or_else(|| o.strip_prefix("wss://"))
        .or_else(|| o.strip_prefix("ws://"))
        .unwrap_or(o)
}

/// The `/editor` WebSocket URL — `wss://` when `secure` (the TLS toggle), else
/// plain `ws://`.
fn ws_url(origin: &str, secure: bool) -> String {
    let scheme = if secure { "wss" } else { "ws" };
    format!("{scheme}://{}/editor", authority(origin))
}

/// The HTTP base for the `/png` side-channel — `https://` when `secure`, else
/// plain `http://`.
fn http_base(origin: &str, secure: bool) -> String {
    let scheme = if secure { "https" } else { "http" };
    format!("{scheme}://{}", authority(origin))
}

async fn run(control_origin: String) -> Result<(), String> {
    let url = ws_url(&control_origin, tls().get());
    tracing::info!("mcp: connecting to {url}");
    let ws = WebSocket::open(&url).map_err(|e| format!("ws open: {e}"))?;
    let (mut sink, mut stream) = ws.split();

    // Outbound frames funnel through one writer (drained below) so concurrent
    // replies/events never interleave a half-written frame.
    let (out_tx, mut out_rx) = mpsc::unbounded::<WsClientMsg>();
    SESSION.with(|s| *s.borrow_mut() = Some(out_tx));
    status().set(RemoteStatus::Connected);
    notify_info("MCP connected");
    tracing::info!("mcp: attached");
    // Seed the server with this tab's visibility (and keep it updated for the
    // life of the page) so agents get fast, explicit "tab hidden" errors instead
    // of full request timeouts when rAF is paused.
    hook_visibility();
    push_visibility();

    loop {
        futures::select! {
            inbound = stream.next().fuse() => match inbound {
                Some(Ok(Message::Text(txt))) => match serde_json::from_str::<WsServerMsg>(&txt) {
                    Ok(WsServerMsg::Request { id, req }) => spawn_local(serve_one(id, req)),
                    Ok(WsServerMsg::Detached) => {
                        // A newer tab attached to this single-session server and
                        // took over — don't fight for it by reconnecting.
                        RECONNECT.with(|r| r.set(false));
                        notify_info("MCP: detached (a newer editor tab took over this server)");
                        return Ok(());
                    }
                    Err(e) => tracing::warn!("mcp: bad frame: {e}"),
                },
                Some(Ok(_)) => {} // non-text frame; ignore
                Some(Err(e)) => return Err(format!("ws read: {e}")),
                None => return Ok(()), // socket closed by server
            },
            outbound = out_rx.next().fuse() => match outbound {
                Some(frame) => {
                    let txt = serde_json::to_string(&frame)
                        .map_err(|e| format!("encode frame: {e}"))?;
                    sink.send(Message::Text(txt))
                        .await
                        .map_err(|e| format!("ws send: {e}"))?;
                }
                None => return Ok(()), // outbound sender dropped → disconnect()
            },
        }
    }
}

/// Serve one decoded request and reply with the matching `id`.
async fn serve_one(id: u64, req: Request) {
    activity_begin();
    let resp = dispatch(req).await;
    send_frame(WsClientMsg::Response { id, resp });
    activity_end().await;
}

/// Follow the agent to the workspace it's editing: when "Follow agent" is on
/// and the command belongs to a different mode than the one on screen, switch
/// to it so the work happens in view. No-op for mode-agnostic commands
/// (`None`) or when already there. Gated by its OWN toggle (default OFF),
/// independent of the activity-feed overlay.
fn follow_agent_mode(mode: Option<awsm_renderer_editor_protocol::EditorMode>) {
    let Some(m) = mode else {
        return;
    };
    if !crate::engine::activity_feed::follow_enabled().get() {
        return;
    }
    let ctrl = controller();
    if ctrl.mode.get() != m {
        ctrl.mode.set_neq(m);
    }
}

/// Interpret a request against the live controller. All editor mutation flows
/// through `EditorController` (the "all via controller" rule).
async fn dispatch(req: Request) -> Response {
    let ctrl = controller();
    match req {
        Request::Mode => Response::Mode(ctrl.mode.get()),
        Request::Dispatch(cmd) => {
            // "Watch-it-work": narrate the agent's command into the activity
            // feed + spotlight the focus panel, and FOLLOW the agent to the
            // workspace it's editing (so a material/animation edit doesn't happen
            // off-screen while you're on the Scene tab). Read-only/informational —
            // never mutates the document; derived from the command alone.
            crate::engine::activity_feed::narrate(&cmd);
            follow_agent_mode(crate::engine::activity_feed::command_mode(&cmd));
            match ctrl.dispatch(cmd).await {
                Ok(()) => {
                    // External (agent) edit → re-seed the inspector's seed-once
                    // property widgets from the mutated node.kind. Local UI edits
                    // don't hit this path, so scrubs are never torn.
                    ctrl.note_external_mutation();
                    Response::Ok
                }
                Err(e) => Response::Err(format!("{e}")),
            }
        }
        Request::DispatchBatch(cmds) => {
            crate::engine::activity_feed::narrate_batch(&cmds);
            follow_agent_mode(
                cmds.iter()
                    .find_map(crate::engine::activity_feed::command_mode),
            );
            match ctrl.dispatch_batch(cmds).await {
                Ok(()) => {
                    ctrl.note_external_mutation();
                    Response::Ok
                }
                Err(e) => Response::Err(format!("{e}")),
            }
        }
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
            png_response(crate::engine::query::scene_png(width, height).await).await
        }
        Request::MaterialPng { width, height } => {
            png_response(crate::engine::query::material_png(width, height)).await
        }
        Request::TexturePng(id) => match crate::engine::query::texture_png(id).await {
            Ok(data_url) => png_from_data_url(&data_url).await,
            Err(e) => Response::Err(e),
        },
        Request::ExportGlb { node } => glb_response(ctrl.export_glb_bytes(node).await).await,
        Request::ExportPlayerBundle => {
            bundle_response(crate::controller::export::bake_player_bundle(&ctrl).await).await
        }
        Request::SaveProject => {
            // Same persisted form a directory Save writes: `project.toml` first
            // (mirrors `assemble_bundle`'s scene.toml-first ordering), then the
            // byte side files sorted by path — the source is a HashMap, and an
            // unsorted manifest would reshuffle on every save.
            let files = crate::controller::persistence::serialize_inmem(&ctrl)
                .map(|(toml, byte_files)| {
                    let mut side: Vec<_> = byte_files.into_iter().collect();
                    side.sort_by(|(a, _), (b, _)| a.cmp(b));
                    std::iter::once(("project.toml".to_string(), toml.into_bytes()))
                        .chain(side)
                        .map(|(path, bytes)| {
                            awsm_renderer_editor_protocol::BundleFile::new(path, bytes)
                        })
                        .collect()
                })
                .map_err(|e| format!("{e}"));
            bundle_response(files).await
        }
    }
}

/// POST exported `.glb` bytes to the server's `/glb/<id>` side-channel (off the
/// control link) and return a [`GlbHandle`]. Mirrors [`png_from_data_url`]: the
/// bytes never ride the link, so a multi-MiB export can't blow the session.
async fn glb_response(result: Result<Vec<u8>, String>) -> Response {
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(e) => return Response::Err(e),
    };
    let byte_len = bytes.len();
    let id = uuid::Uuid::new_v4().to_string();
    let origin = ORIGIN.with(|o| o.get_cloned());
    if let Err(e) = upload_bytes(&origin, "glb", &id, bytes).await {
        return Response::Err(format!("glb upload failed: {e}"));
    }
    Response::Glb(GlbHandle { id, byte_len })
}

/// POST a baked player bundle's files to the server's `/bundle/<id>/<path>`
/// side-channel (off the control link) and return a [`BundleHandle`] manifest.
/// Mirrors [`glb_response`]: only the manifest crosses the link, so a
/// multi-file, multi-MiB bundle can't blow the session (or the agent's token
/// stream — this replaced an inline-base64 query result that did exactly that).
async fn bundle_response(
    result: Result<Vec<awsm_renderer_editor_protocol::BundleFile>, String>,
) -> Response {
    let files = match result {
        Ok(files) => files,
        Err(e) => return Response::Err(e),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let origin = ORIGIN.with(|o| o.get_cloned());
    let mut metas = Vec::with_capacity(files.len());
    for f in files {
        let byte_len = f.bytes.len();
        // Percent-encode each segment so a path lands intact in the URL; the
        // server decodes and re-validates it before touching the filesystem.
        let encoded = f
            .path
            .split('/')
            .map(|seg| String::from(js_sys::encode_uri_component(seg)))
            .collect::<Vec<_>>()
            .join("/");
        if let Err(e) = upload_bytes(&origin, "bundle", &format!("{id}/{encoded}"), f.bytes).await {
            return Response::Err(format!("bundle upload failed ({}): {e}", f.path));
        }
        metas.push(BundleFileMeta {
            path: f.path,
            byte_len,
        });
    }
    Response::Bundle(BundleHandle { id, files: metas })
}

/// Turn an optional `data:image/png;base64,…` URL into a [`Response::Png`] handle
/// (uploading the bytes to the side-channel), or an error when none is available.
async fn png_response(opt: Option<String>) -> Response {
    match opt {
        Some(data_url) if !data_url.is_empty() => png_from_data_url(&data_url).await,
        _ => Response::Err("no image available".to_string()),
    }
}

/// Decode a `data:image/png;base64,…` URL, POST the raw bytes to the server's
/// `/png/<id>` side-channel (off the control link), and return a [`PngHandle`].
async fn png_from_data_url(data_url: &str) -> Response {
    let Some((_, b64)) = data_url.split_once(',') else {
        return Response::Err("malformed png data url".to_string());
    };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) {
        Ok(bytes) => bytes,
        Err(e) => return Response::Err(format!("png base64 decode: {e}")),
    };
    let (width, height) = png_dimensions(&bytes).unwrap_or((0, 0));
    let byte_len = bytes.len();
    let id = uuid::Uuid::new_v4().to_string();
    let origin = ORIGIN.with(|o| o.get_cloned());
    if let Err(e) = upload_bytes(&origin, "png", &id, bytes).await {
        return Response::Err(format!("png upload failed: {e}"));
    }
    Response::Png(PngHandle {
        id,
        byte_len,
        width,
        height,
    })
}

/// Parse a PNG's pixel dimensions from its IHDR chunk: the 8-byte signature, then
/// a length+type header, then width/height as big-endian `u32`s at byte offsets
/// 16 / 20. Returns `None` if the buffer is too short or isn't a PNG.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

/// POST raw bytes to `<origin>/<kind>/<id>` (`kind` is `png` or `glb`) over plain
/// HTTP — a separate connection from the control link, so a multi-MiB payload
/// never blocks small frames. Posting *before* replying guarantees the server has
/// the bytes by the time it sees the handle.
async fn upload_bytes(origin: &str, kind: &str, id: &str, bytes: Vec<u8>) -> Result<(), String> {
    let url = format!("{}/{kind}/{id}", http_base(origin, tls().get()));
    let body = js_sys::Uint8Array::from(bytes.as_slice());
    let resp = gloo_net::http::Request::post(&url)
        .header("content-type", "application/octet-stream")
        .body(body)
        .map_err(|e| format!("build request: {e}"))?
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    if !resp.ok() {
        return Err(format!("server returned HTTP {}", resp.status()));
    }
    Ok(())
}
