//! Sanity checks: `EditorProject` round-trips through both serde-JSON
//! (the on-disk `project.json` format) and bitcode (the per-game
//! build artifact). If either of these regresses, the editor's
//! Save/Load or the runtime's bin-loading will silently break.

use super::*;

fn sample() -> EditorProject {
    let asset = AssetId::new();
    let mut assets = AssetTable::new();
    // Content-hash addressing post-`feat(schema): content-hash …`:
    // a stable test hash keeps this round-trip deterministic across
    // serializers; real callers compute the SHA-256 from upload bytes.
    assets.insert_file_with_hash(
        "robot.glb".to_string(),
        "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
    );
    let _unused = asset; // keep the fresh-id helper exercised
    EditorProject {
        name: String::new(),
        environment: EnvironmentConfig::default(),
        shadows: Default::default(),
        assets,
        custom_materials: Vec::new(),
        editor_materials: Vec::new(),
        custom_animations: Vec::new(),
        editor_animations: Vec::new(),
        anim_mixer: Default::default(),
        nodes: vec![EditorNode {
            id: NodeId::new(),
            name: "root".to_string(),
            transform: Trs::IDENTITY,
            kind: NodeKind::Group,
            locked: false,
            visible: true,
            prefab: false,
            children: vec![],
        }],
    }
}

#[test]
fn json_roundtrip() {
    let project = sample();
    let json = serde_json::to_string(&project).unwrap();
    let back: EditorProject = serde_json::from_str(&json).unwrap();
    assert_eq!(project, back);
}

#[test]
fn bitcode_roundtrip() {
    let project = sample();
    let bytes = bitcode::serialize(&project).unwrap();
    let back: EditorProject = bitcode::deserialize(&bytes).unwrap();
    assert_eq!(project, back);
}

/// The captured-mesh side-file bytes (`assets/<id>.mesh.bin`) are bitcode; this
/// is the format the editor/player read back, so guard the round-trip.
#[test]
fn captured_mesh_bitcode_roundtrip() {
    let mesh = CapturedMesh {
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
        uvs: Some(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
        colors: None,
        indices: vec![0, 1, 2],
    };
    let bytes = bitcode::serialize(&mesh).unwrap();
    let back: CapturedMesh = bitcode::deserialize(&bytes).unwrap();
    assert_eq!(mesh, back);
}

/// `MeshDef.editable` defaults to `false` for pre-feature files, and the new
/// `CapturedSource` variants round-trip through serde + bitcode.
#[test]
fn mesh_def_editable_default_and_sources() {
    // Old files have no `editable` key → defaults false.
    let old: MeshDef = serde_json::from_str(r#"{"label":"m"}"#).unwrap();
    assert!(!old.editable);

    for src in [
        None,
        Some(CapturedSource::Editable),
        Some(CapturedSource::Imported {
            source: AssetId::new(),
        }),
    ] {
        let def = MeshDef {
            label: "m".to_string(),
            source: src,
            editable: true,
        };
        let json = serde_json::to_string(&def).unwrap();
        assert_eq!(def, serde_json::from_str::<MeshDef>(&json).unwrap());
        let bin = bitcode::serialize(&def).unwrap();
        assert_eq!(def, bitcode::deserialize::<MeshDef>(&bin).unwrap());
    }
}
