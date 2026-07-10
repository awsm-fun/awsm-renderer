//! Wire-encoding regression guard.
//!
//! `EditorCommand` is internally tagged (`#[serde(tag = "cmd")]`), and serde
//! CANNOT serialize an internally-tagged newtype variant whose payload is a
//! sequence — which is exactly `EditorCommand::Batch(Vec<EditorCommand>)`.
//! The ws writer (`src/ws.rs`) silently drops frames that fail to serialize,
//! so a `Request::Dispatch(EditorCommand::Batch(..))` never reaches the tab
//! and the caller burns the full request timeout (the original
//! `insert_instancer{mesh}` bug). `Batch` is an IN-PROCESS grouping only;
//! anything crossing the wire must ride `Request::DispatchBatch` instead.

use awsm_renderer_editor_protocol::{EditorCommand, NodeId, Request};

/// `Batch` does not survive the wire — pin the failure so nobody "fixes" a
/// timeout by wrapping commands in it again.
#[test]
fn batch_command_is_not_wire_serializable() {
    let cmd = EditorCommand::Batch(vec![EditorCommand::Delete { id: NodeId::new() }]);
    let err = serde_json::to_string(&Request::Dispatch(cmd));
    assert!(
        err.is_err(),
        "EditorCommand::Batch unexpectedly serialized — if serde/the tagging \
         changed, insert_instancer can go back to a single Batch dispatch"
    );
}

/// The supported wire form for atomic multi-command steps.
#[test]
fn dispatch_batch_request_is_wire_serializable() {
    let cmds = vec![
        EditorCommand::Delete { id: NodeId::new() },
        EditorCommand::Delete { id: NodeId::new() },
    ];
    let txt = serde_json::to_string(&Request::DispatchBatch(cmds)).expect("must serialize");
    let back: Request = serde_json::from_str(&txt).expect("must round-trip");
    assert!(matches!(back, Request::DispatchBatch(v) if v.len() == 2));
}
