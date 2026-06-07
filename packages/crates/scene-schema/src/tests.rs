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
