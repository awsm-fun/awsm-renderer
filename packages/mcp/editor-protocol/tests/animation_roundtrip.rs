//! Serde round-trip for animation `EditorCommand` variants added by the
//! mcp-test-fixes plan. Commands cross the WS link as serde-tagged JSON
//! (`#[serde(tag = "cmd")]`).

use awsm_renderer_editor_protocol::{AssetId, EditorCommand, NodeId};

#[test]
fn add_spin_track_command_json_roundtrip() {
    let cmd = EditorCommand::AddSpinTrack {
        clip: AssetId::new(),
        node: NodeId::new(),
        axis: [0.0, 1.0, 0.0],
        turns: 2.0,
        duration: 1.5,
        keys_per_turn: Some(6),
    };
    let json = serde_json::to_string(&cmd).expect("serialize");
    assert!(
        json.contains("\"cmd\":\"add_spin_track\""),
        "tag missing: {json}"
    );
    let back: EditorCommand = serde_json::from_str(&json).expect("deserialize");
    match back {
        EditorCommand::AddSpinTrack {
            axis,
            turns,
            duration,
            keys_per_turn,
            ..
        } => {
            assert_eq!(axis, [0.0, 1.0, 0.0]);
            assert_eq!(turns, 2.0);
            assert_eq!(duration, 1.5);
            assert_eq!(keys_per_turn, Some(6));
        }
        other => panic!("expected AddSpinTrack, got {other:?}"),
    }
}

#[test]
fn add_spin_track_keys_per_turn_defaults_to_none() {
    // keys_per_turn is #[serde(default)] — omitting it deserializes to None
    // (the handler then uses 4).
    let json = format!(
        "{{\"cmd\":\"add_spin_track\",\"clip\":\"{}\",\"node\":\"{}\",\"axis\":[1,0,0],\"turns\":1.0,\"duration\":2.0}}",
        AssetId::new(),
        NodeId::new()
    );
    let back: EditorCommand = serde_json::from_str(&json).expect("deserialize");
    match back {
        EditorCommand::AddSpinTrack { keys_per_turn, .. } => assert_eq!(keys_per_turn, None),
        other => panic!("expected AddSpinTrack, got {other:?}"),
    }
}
