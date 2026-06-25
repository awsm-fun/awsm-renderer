//! Serde round-trip coverage for `EditorQuery` variants added by the
//! mcp-test-fixes plan. Queries cross the WS link as serde-tagged JSON
//! (`#[serde(tag = "query")]`); a tag/field drift would make the MCP tool
//! silently fail to reach the editor.

use awsm_renderer_editor_protocol::{EditorQuery, NodeId};

#[test]
fn get_mesh_data_query_json_roundtrip() {
    let q = EditorQuery::GetMeshData {
        node: NodeId::new(),
        offset: Some(3),
        limit: Some(64),
    };
    let json = serde_json::to_string(&q).expect("serialize");
    assert!(
        json.contains("\"query\":\"get_mesh_data\""),
        "tag missing: {json}"
    );
    let back: EditorQuery = serde_json::from_str(&json).expect("deserialize");
    match back {
        EditorQuery::GetMeshData { offset, limit, .. } => {
            assert_eq!(offset, Some(3));
            assert_eq!(limit, Some(64));
        }
        other => panic!("expected GetMeshData, got {other:?}"),
    }
}

#[test]
fn get_mesh_data_defaults_when_paging_omitted() {
    // offset/limit are `#[serde(default)]` — omitting them must deserialize to None
    // (read the whole index buffer).
    let json = format!(
        "{{\"query\":\"get_mesh_data\",\"node\":\"{}\"}}",
        NodeId::new()
    );
    let back: EditorQuery = serde_json::from_str(&json).expect("deserialize");
    match back {
        EditorQuery::GetMeshData { offset, limit, .. } => {
            assert_eq!(offset, None);
            assert_eq!(limit, None);
        }
        other => panic!("expected GetMeshData, got {other:?}"),
    }
}

#[test]
fn strip_parameterize_query_roundtrip() {
    let q = EditorQuery::StripParameterize {
        node: NodeId::new(),
        selection: Some(7),
        indices: vec![],
        axis: Some([0.0, 1.0, 0.0]),
    };
    let json = serde_json::to_string(&q).expect("serialize");
    assert!(
        json.contains("\"query\":\"strip_parameterize\""),
        "tag missing: {json}"
    );
    let back: EditorQuery = serde_json::from_str(&json).expect("deserialize");
    match back {
        EditorQuery::StripParameterize {
            selection, axis, ..
        } => {
            assert_eq!(selection, Some(7));
            assert_eq!(axis, Some([0.0, 1.0, 0.0]));
        }
        other => panic!("expected StripParameterize, got {other:?}"),
    }

    // Omitting selection/indices/axis defaults cleanly (whole-mesh, auto-axis).
    let json = format!(
        "{{\"query\":\"strip_parameterize\",\"node\":\"{}\"}}",
        NodeId::new()
    );
    let back: EditorQuery = serde_json::from_str(&json).expect("deserialize");
    match back {
        EditorQuery::StripParameterize {
            selection,
            indices,
            axis,
            ..
        } => {
            assert_eq!(selection, None);
            assert!(indices.is_empty());
            assert_eq!(axis, None);
        }
        other => panic!("expected StripParameterize, got {other:?}"),
    }
}

#[test]
fn get_vertex_data_include_source_roundtrip() {
    // The new `include_source` flag round-trips and defaults to false (compact).
    let q = EditorQuery::GetVertexData {
        node: NodeId::new(),
        indices: vec![0, 5],
        selection: None,
        offset: None,
        limit: None,
        include_source: true,
    };
    let json = serde_json::to_string(&q).expect("serialize");
    let back: EditorQuery = serde_json::from_str(&json).expect("deserialize");
    match back {
        EditorQuery::GetVertexData { include_source, .. } => assert!(include_source),
        other => panic!("expected GetVertexData, got {other:?}"),
    }

    // Omitting the flag defaults to false.
    let json = format!(
        "{{\"query\":\"get_vertex_data\",\"node\":\"{}\",\"indices\":[1]}}",
        NodeId::new()
    );
    let back: EditorQuery = serde_json::from_str(&json).expect("deserialize");
    match back {
        EditorQuery::GetVertexData { include_source, .. } => assert!(!include_source),
        other => panic!("expected GetVertexData, got {other:?}"),
    }
}
