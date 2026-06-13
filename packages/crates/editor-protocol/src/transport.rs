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

#[cfg(test)]
mod wire_roundtrip_tests {
    //! The MCP / `/debug` wire is serde_json: the native server serializes a
    //! `Request`, the editor deserializes it (and vice-versa for `Response`).
    //! A serde rename / tag drift would silently break every agent tool with no
    //! compile error. These round-trip a representative slice of the actively-
    //! used command + query surface and assert ser→de→ser is idempotent (which
    //! also proves deserialize accepts what serialize produced — the failure
    //! mode that bites over the wire). Complex-payload variants (MaterialDef /
    //! Trs) are exercised continuously by the live MCP path; this guards the
    //! envelope + the simple high-traffic verbs against rename drift.

    use super::*;
    use awsm_scene::{MaterialShading, NodeId};

    /// ser → de → ser must be byte-stable, and the round-tripped value must
    /// re-serialize to the SAME JSON (catches asymmetric/lossy serde + a
    /// deserialize that silently drops or renames a field).
    fn assert_roundtrips(req: &Request, label: &str) {
        let j1 =
            serde_json::to_string(req).unwrap_or_else(|e| panic!("{label}: serialize failed: {e}"));
        let back: Request = serde_json::from_str(&j1).unwrap_or_else(|e| {
            panic!("{label}: deserialize of own output failed: {e}\njson={j1}")
        });
        let j2 = serde_json::to_string(&back)
            .unwrap_or_else(|e| panic!("{label}: re-serialize failed: {e}"));
        assert_eq!(j1, j2, "{label}: round-trip not idempotent");
    }

    #[test]
    fn representative_commands_roundtrip() {
        let cmds = [
            (
                "switch_mode",
                EditorCommand::SwitchMode {
                    mode: EditorMode::Animation,
                },
            ),
            (
                "set_selection",
                EditorCommand::SetSelection {
                    ids: vec![NodeId::new()],
                },
            ),
            ("add_clip", EditorCommand::AddClip { id: AssetId::new() }),
            (
                "delete_clip",
                EditorCommand::DeleteClip { id: AssetId::new() },
            ),
            ("set_playhead", EditorCommand::SetPlayhead { t: 0.35 }),
            ("set_playing", EditorCommand::SetPlaying { on: true }),
            ("set_anim_fps", EditorCommand::SetAnimFps { fps: 30 }),
            (
                "set_morph_weight",
                EditorCommand::SetMorphWeight {
                    node: NodeId::new(),
                    index: 2,
                    value: 0.5,
                },
            ),
            (
                "add_builtin_material",
                EditorCommand::AddBuiltinMaterial {
                    id: AssetId::new(),
                    shading: MaterialShading::Pbr,
                },
            ),
        ];
        for (label, cmd) in cmds {
            // The serde tag must be the snake_case `cmd` discriminator.
            let j = serde_json::to_string(&cmd).unwrap();
            assert!(
                j.contains(&format!("\"cmd\":\"{label}\"")),
                "command tag drift: expected cmd=\"{label}\" in {j}",
            );
            assert_roundtrips(&Request::Dispatch(cmd), label);
        }
    }

    #[test]
    fn representative_queries_roundtrip() {
        let queries = [
            ("snapshot", EditorQuery::Snapshot),
            ("frame_globals", EditorQuery::FrameGlobals),
            ("memory_stats", EditorQuery::MemoryStats),
            ("console_logs", EditorQuery::ConsoleLogs { limit: 25 }),
            (
                "get_skin_weights",
                EditorQuery::GetSkinWeights {
                    node: NodeId::new(),
                    indices: vec![0, 1, 2],
                },
            ),
        ];
        for (label, q) in queries {
            assert_roundtrips(&Request::Query(q), label);
        }
    }

    #[test]
    fn envelope_variants_roundtrip() {
        assert_roundtrips(&Request::Undo, "undo");
        assert_roundtrips(&Request::Redo, "redo");
        assert_roundtrips(
            &Request::ScenePng {
                width: Some(900),
                height: Some(600),
            },
            "scene_png",
        );
        assert_roundtrips(&Request::Mode, "mode");
    }
}
