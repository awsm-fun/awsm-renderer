//! The request/response envelope exchanged over the WebTransport link between
//! the native MCP server and the in-browser editor.
//!
//! One request travels per server-initiated bidirectional stream (the server
//! `open_bi`s, the editor `accept_bi`s) and the editor replies on the same
//! stream — so there is no request-id correlation: stream identity *is* the
//! correlation, and framing is by stream-finish (write the whole message, then
//! `finish()`; read to end, then decode). Encoded with `bitcode` at the
//! transport edges (PNG bytes stay raw).

use serde::{Deserialize, Serialize};

use awsm_scene::AssetId;

use crate::{EditorCommand, EditorMode, EditorQuery, QueryResult};

/// Server → editor. What the editor should do / report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Apply a mutation through `EditorController::dispatch`.
    Dispatch(EditorCommand),
    /// Apply a list of mutations in order as one atomic undo step
    /// (`EditorController::dispatch_batch`). One round-trip, one undo entry.
    DispatchBatch(Vec<EditorCommand>),
    /// Run a read-only `EditorQuery`.
    Query(EditorQuery),
    /// Undo the last recorded command (a controller method, not a command).
    Undo,
    /// Redo the last undone command.
    Redo,
    /// PNG of the scene viewport (raw bytes, not a data: URL). Optional
    /// `width`/`height` scale the output (the source is the live viewport;
    /// scaling normalizes size / trims tokens, it doesn't add detail).
    ScenePng {
        width: Option<u32>,
        height: Option<u32>,
    },
    /// PNG of the material-mode preview sphere (raw bytes). Optional output size.
    MaterialPng {
        width: Option<u32>,
        height: Option<u32>,
    },
    /// PNG of a texture asset thumbnail (raw bytes).
    TexturePng(AssetId),
    /// The current workspace mode.
    Mode,
}

/// Editor → server **push** event (the unsolicited channel, distinct from the
/// request/response path). The editor opens a unidirectional stream per event;
/// the server relays it to the connected agent as an MCP logging notification.
/// Carries compile/runtime notices (toasts) and selection changes so an agent
/// can react to what a human (or async work) did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorEvent {
    /// Event kind: `"toast"` | `"selection"`.
    pub kind: String,
    /// Toast severity (`"info"` | `"warning"` | `"error"`) for `kind == "toast"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Human-readable message (toast text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Selected node ids for `kind == "selection"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<String>>,
}

/// Editor → server. The reply to a [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// A mutation / control op succeeded with no payload.
    Ok,
    /// A query result (boxed — `QueryResult::Snapshot` is large, and serde boxes
    /// transparently so the JSON wire form is unchanged).
    Query(Box<QueryResult>),
    /// Raw PNG bytes.
    Png(Vec<u8>),
    /// The current workspace mode.
    Mode(EditorMode),
    /// The request failed; the string is a human-readable reason.
    Err(String),
}
