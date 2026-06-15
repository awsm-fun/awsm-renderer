//! The link to the attached editor over the `/editor` WebSocket.
//!
//! The server holds at most one attached editor (one socket). Each request is
//! tagged with an `id`; the editor replies with a [`Response`] carrying the same
//! id (the link is one ordered channel, so ids correlate request↔response). The
//! single writer task that owns the socket sink lives in [`crate::ws`]; here we
//! track the per-connection pending-request map + the outbound sender, plus the
//! broadcast of editor push events to every MCP session's forwarder.
//!
//! Per-tab isolation (pairing codes, multiple concurrent editors) is layered on
//! in a later phase; today the most-recently-attached tab wins and every MCP
//! agent shares it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::{broadcast, mpsc, oneshot};

use awsm_editor_protocol::{EditorEvent, Request, Response, WsServerMsg};

/// Upper bound on one request's round-trip (an offline render / settle is the
/// slow case).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// One attached editor tab (one `/editor` socket), with its own request-id space
/// and pending-request map so a frame can only ever complete its own request.
pub struct Connection {
    pub id: u64,
    tx: mpsc::UnboundedSender<WsServerMsg>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Response>>>,
    next_req_id: AtomicU64,
}

impl Connection {
    /// Send one request to this tab and await its response.
    async fn request(&self, req: &Request) -> Result<Response> {
        let id = self.next_req_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.tx
            .send(WsServerMsg::Request {
                id,
                req: req.clone(),
            })
            .map_err(|_| anyhow!("editor link closed"))?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("editor dropped the request"))
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("editor request timed out"))
            }
        }
    }

    /// Complete a pending request from an incoming `Response` frame.
    pub fn complete(&self, id: u64, resp: Response) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(resp);
        }
    }

    /// Fail every in-flight request (on socket close): dropping the senders makes
    /// each awaiting `request` resolve to the "dropped" error.
    fn drain(&self) {
        self.pending.lock().unwrap().clear();
    }
}

struct LinkInner {
    /// The single attached editor tab, if any (most-recent-attach wins).
    conn: Mutex<Option<Arc<Connection>>>,
    /// Fan-out of editor push events to every connected MCP session's forwarder.
    events: broadcast::Sender<EditorEvent>,
    next_conn_id: AtomicU64,
}

/// Shared handle to the attached-editor registry. Cheap to clone (`Arc`).
#[derive(Clone)]
pub struct EditorLink {
    inner: Arc<LinkInner>,
}

impl EditorLink {
    pub fn shared() -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(LinkInner {
                conn: Mutex::new(None),
                events,
                next_conn_id: AtomicU64::new(1),
            }),
        }
    }

    /// Register a freshly-attached tab as the active editor, returning its
    /// [`Connection`]. Replaces any previous attachment (last-attach wins).
    pub fn register_connection(&self, tx: mpsc::UnboundedSender<WsServerMsg>) -> Arc<Connection> {
        let id = self.inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
        let conn = Arc::new(Connection {
            id,
            tx,
            pending: Mutex::new(HashMap::new()),
            next_req_id: AtomicU64::new(1),
        });
        if let Some(old) = self.inner.conn.lock().unwrap().replace(conn.clone()) {
            old.drain();
        }
        conn
    }

    /// Remove a tab on socket close (only if it's still the active one — a newer
    /// tab may already have taken over): fail its in-flight requests and forget it.
    pub fn remove_connection(&self, id: u64) {
        let mut guard = self.inner.conn.lock().unwrap();
        if guard.as_ref().is_some_and(|c| c.id == id) {
            if let Some(conn) = guard.take() {
                conn.drain();
            }
        }
    }

    /// Is an editor currently attached? (For `GET /health`.)
    pub fn is_attached(&self) -> bool {
        self.inner.conn.lock().unwrap().is_some()
    }

    /// Publish an editor push event to all subscribed MCP forwarders. (Called by
    /// the WebSocket reader in [`crate::ws`].)
    pub fn publish_event(&self, ev: EditorEvent) {
        // Err only when there are no receivers — fine, just drop it.
        let _ = self.inner.events.send(ev);
    }

    /// Subscribe to the editor push-event stream (one per MCP session forwarder).
    pub fn subscribe_events(&self) -> broadcast::Receiver<EditorEvent> {
        self.inner.events.subscribe()
    }

    /// Send a request to the attached editor and await its response. Errors when
    /// no editor is attached. (Used by the rmcp tool layer + the `/debug` seam.)
    pub async fn request(&self, req: &Request) -> Result<Response> {
        let conn = self
            .inner
            .conn
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("no editor attached"))?;
        conn.request(req).await
    }
}
