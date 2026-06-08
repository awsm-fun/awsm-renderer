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

use awsm_scene_schema::AssetId;

use crate::{EditorCommand, EditorMode, EditorQuery, QueryResult};

/// Server → editor. What the editor should do / report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Apply a mutation through `EditorController::dispatch`.
    Dispatch(EditorCommand),
    /// Run a read-only `EditorQuery`.
    Query(EditorQuery),
    /// Undo the last recorded command (a controller method, not a command).
    Undo,
    /// Redo the last undone command.
    Redo,
    /// PNG of the scene viewport (raw bytes, not a data: URL).
    ScenePng,
    /// PNG of the material-mode preview sphere (raw bytes).
    MaterialPng,
    /// PNG of a texture asset thumbnail (raw bytes).
    TexturePng(AssetId),
    /// The current workspace mode.
    Mode,
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
