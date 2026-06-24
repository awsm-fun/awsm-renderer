//! The link between MCP agents and attached editor tabs.
//!
//! Two independent identities meet here:
//!   - a [`Connection`] is one editor tab (one `/editor` WebSocket), with its own
//!     request-id space, pending-request map, and writer — so a frame from one
//!     tab can never complete another's request.
//!   - an [`AgentSession`] is one MCP client (one `EditorMcp`), with a pairing
//!     code.
//!
//! Each agent is *bound* to one connection. A request from a bound agent goes
//! only to its tab; an event from a tab is delivered only to its bound agent.
//! Binding is automatic when exactly one unbound tab and one unbound agent exist;
//! otherwise the agent surfaces a pairing code the editor presents (via `?pair=`
//! or the connect modal) to claim the binding. Cross-talk is structurally
//! impossible — there is no shared id space and no shared link.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, oneshot};

use awsm_renderer_editor_protocol::{EditorEvent, Request, Response, WsServerMsg};

/// Upper bound on one request's round-trip (an offline render / settle is the
/// slow case).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Crockford base32 (no I/L/O/U) — unambiguous when read aloud / typed.
const PAIR_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const PAIR_CODE_LEN: usize = 4;

/// Why a request couldn't be delivered.
pub enum LinkError {
    /// No tab is bound and binding is ambiguous — the editor must present this
    /// pairing code first.
    PairingRequired(String),
    /// The bound tab's link failed (closed, dropped, or timed out).
    Transport(String),
}

/// One attached editor tab.
pub struct Connection {
    pub id: u64,
    tx: mpsc::UnboundedSender<WsServerMsg>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Response>>>,
    next_req_id: AtomicU64,
    /// The agent bound to this tab, if any. A `Weak` so a dropped / timed-out
    /// agent frees the tab automatically — a new agent can then auto-bind to it
    /// (self-healing).
    bound_agent: Mutex<Option<Weak<AgentSession>>>,
}

impl Connection {
    /// Send one request to this tab and await its response.
    async fn request(&self, req: &Request) -> Result<Response, String> {
        let id = self.next_req_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.tx
            .send(WsServerMsg::Request {
                id,
                req: req.clone(),
            })
            .map_err(|_| "editor link closed".to_string())?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => {
                self.pending.lock().unwrap().remove(&id);
                Err("editor dropped the request".into())
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(
                    "editor request timed out — is the editor tab foregrounded? \
                     A screenshot/render needs a live requestAnimationFrame frame, \
                     which browsers throttle or pause in a backgrounded/hidden tab. \
                     Foreground the tab (or keep it visible) and retry."
                        .into(),
                )
            }
        }
    }

    /// Complete a pending request from an incoming `Response` frame.
    pub fn complete(&self, id: u64, resp: Response) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(resp);
        }
    }

    /// Push a server→browser frame (best-effort).
    pub fn send(&self, msg: WsServerMsg) {
        let _ = self.tx.send(msg);
    }

    /// Fail every in-flight request (on socket close): dropping the senders makes
    /// each awaiting `request` resolve to the "dropped" error.
    fn drain(&self) {
        self.pending.lock().unwrap().clear();
    }
}

/// One connected MCP agent (one `EditorMcp`, one `Mcp-Session-Id`).
pub struct AgentSession {
    pub id: u64,
    pub pair_code: String,
    bound_conn: Mutex<Option<Weak<Connection>>>,
}

impl AgentSession {
    /// The id of the tab this agent is currently bound to (if the binding is
    /// still alive). Used to filter the event stream.
    pub fn bound_conn_id(&self) -> Option<u64> {
        self.bound_conn
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Weak::upgrade)
            .map(|c| c.id)
    }
}

struct LinkInner {
    connections: Mutex<Vec<Arc<Connection>>>,
    agents: Mutex<Vec<Weak<AgentSession>>>,
    /// Editor push events, tagged with the originating connection id so each
    /// agent forwarder can keep only its bound tab's events.
    events: broadcast::Sender<(u64, EditorEvent)>,
    /// This server's own origin (e.g. `http://127.0.0.1:9086`), surfaced in the
    /// `?mcp=…&pair=…` hint the pairing tools hand the agent.
    self_origin: String,
    next_conn_id: AtomicU64,
    next_agent_id: AtomicU64,
}

/// Shared handle to the agent/connection registry. Cheap to clone (`Arc`).
#[derive(Clone)]
pub struct EditorLink {
    inner: Arc<LinkInner>,
}

impl EditorLink {
    pub fn shared(self_origin: String) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(LinkInner {
                connections: Mutex::new(Vec::new()),
                agents: Mutex::new(Vec::new()),
                events,
                self_origin,
                next_conn_id: AtomicU64::new(1),
                next_agent_id: AtomicU64::new(1),
            }),
        }
    }

    /// This server's origin (for building the `?mcp=…&pair=…` pairing hint).
    pub fn self_origin(&self) -> &str {
        &self.inner.self_origin
    }

    // ── connections (editor tabs) ───────────────────────────────────────────

    /// Register a freshly-attached tab, returning its [`Connection`].
    pub fn register_connection(&self, tx: mpsc::UnboundedSender<WsServerMsg>) -> Arc<Connection> {
        let id = self.inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
        let conn = Arc::new(Connection {
            id,
            tx,
            pending: Mutex::new(HashMap::new()),
            next_req_id: AtomicU64::new(1),
            bound_agent: Mutex::new(None),
        });
        self.inner.connections.lock().unwrap().push(conn.clone());
        conn
    }

    /// Remove a tab on socket close: fail its in-flight requests and forget it.
    pub fn remove_connection(&self, id: u64) {
        let mut conns = self.inner.connections.lock().unwrap();
        if let Some(pos) = conns.iter().position(|c| c.id == id) {
            let conn = conns.remove(pos);
            conn.drain();
        }
    }

    /// How many editor tabs are currently connected (for `pairing_status`).
    pub fn connection_count(&self) -> usize {
        self.inner.connections.lock().unwrap().len()
    }

    /// How many agent sessions are currently alive (for `pairing_status`).
    pub fn agent_count(&self) -> usize {
        self.inner
            .agents
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.upgrade().is_some())
            .count()
    }

    /// Publish an editor push event (tagged with its originating connection).
    pub fn publish_event(&self, conn_id: u64, ev: EditorEvent) {
        // Err only when there are no receivers — fine, just drop it.
        let _ = self.inner.events.send((conn_id, ev));
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<(u64, EditorEvent)> {
        self.inner.events.subscribe()
    }

    // ── agents (MCP sessions) ───────────────────────────────────────────────

    /// Register a new agent session (called once per `EditorMcp`).
    pub fn register_agent(&self) -> Arc<AgentSession> {
        let id = self.inner.next_agent_id.fetch_add(1, Ordering::Relaxed);
        let agent = Arc::new(AgentSession {
            id,
            pair_code: gen_pair_code(),
            bound_conn: Mutex::new(None),
        });
        tracing::debug!(
            "agent session {} registered (pair code {})",
            agent.id,
            agent.pair_code
        );
        let mut agents = self.inner.agents.lock().unwrap();
        agents.retain(|w| w.strong_count() > 0); // prune dead sessions
        agents.push(Arc::downgrade(&agent));
        agent
    }

    /// Count agents that are alive and not yet bound to a tab.
    fn unbound_agent_count(&self) -> usize {
        self.inner
            .agents
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|a| a.bound_conn_id().is_none())
            .count()
    }

    /// Bind `agent` to `conn` (mutually, both via `Weak`).
    fn bind(&self, agent: &Arc<AgentSession>, conn: &Arc<Connection>) {
        *agent.bound_conn.lock().unwrap() = Some(Arc::downgrade(conn));
        *conn.bound_agent.lock().unwrap() = Some(Arc::downgrade(agent));
    }

    /// Is this tab free to auto-bind? True when it has no agent, or its bound
    /// agent has gone away.
    fn conn_is_unbound(conn: &Connection) -> bool {
        conn.bound_agent
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_none()
    }

    /// Resolve the tab an agent should talk to: its live binding, else an
    /// automatic bind when exactly one unbound tab and one unbound agent exist,
    /// else [`LinkError::PairingRequired`].
    pub fn resolve(&self, agent: &Arc<AgentSession>) -> Result<Arc<Connection>, LinkError> {
        // Live existing binding?
        if let Some(weak) = agent.bound_conn.lock().unwrap().clone() {
            if let Some(conn) = weak.upgrade() {
                return Ok(conn);
            }
        }
        // Try to auto-bind.
        let unbound: Vec<Arc<Connection>> = self
            .inner
            .connections
            .lock()
            .unwrap()
            .iter()
            .filter(|c| Self::conn_is_unbound(c))
            .cloned()
            .collect();
        if unbound.len() == 1 && self.unbound_agent_count() == 1 {
            self.bind(agent, &unbound[0]);
            return Ok(unbound[0].clone());
        }
        Err(LinkError::PairingRequired(agent.pair_code.clone()))
    }

    /// Editor-initiated bind: a tab presents a pairing code to claim the agent
    /// that owns it. Returns whether a matching agent was found.
    pub fn bind_by_code(&self, conn: &Arc<Connection>, code: &str) -> bool {
        let code = code.trim().to_uppercase();
        let agent = self
            .inner
            .agents
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .find(|a| a.pair_code == code);
        match agent {
            Some(agent) => {
                // Free this tab's previous agent (if any) and bind the new one.
                self.bind(&agent, conn);
                true
            }
            None => false,
        }
    }

    /// Send a request from `agent` to its bound tab.
    pub async fn request(
        &self,
        agent: &Arc<AgentSession>,
        req: &Request,
    ) -> Result<Response, LinkError> {
        let conn = self.resolve(agent)?;
        conn.request(req).await.map_err(LinkError::Transport)
    }

    /// Best-effort request for the dev `/debug` seam (no agent): use the only /
    /// most-recently-attached tab.
    pub async fn debug_request(&self, req: &Request) -> Result<Response, String> {
        let conn = self
            .inner
            .connections
            .lock()
            .unwrap()
            .last()
            .cloned()
            .ok_or_else(|| "no editor attached".to_string())?;
        conn.request(req).await
    }
}

/// A short, human-typable pairing code.
fn gen_pair_code() -> String {
    let bytes = uuid::Uuid::new_v4();
    let b = bytes.as_bytes();
    (0..PAIR_CODE_LEN)
        .map(|i| PAIR_ALPHABET[(b[i] as usize) % PAIR_ALPHABET.len()] as char)
        .collect()
}
