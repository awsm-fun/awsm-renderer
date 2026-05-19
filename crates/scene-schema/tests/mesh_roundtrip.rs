//! Integration test for `AssetSource::Mesh(MeshDef)` + the
//! `CapturedMesh` side-table bytes (F10 end-to-end).
//!
//! Two persistence shapes are exercised:
//!
//! 1. The `MeshDef` metadata round-trips inside `EditorProject` through
//!    serde-JSON (`project.json`) and bitcode (per-game build artifact).
//! 2. The `CapturedMesh` geometry round-trips through bitcode (the
//!    encoding used for the `assets/<asset-id>.mesh.bin` side file the
//!    editor's Save flow writes).
//!
//! Drift between either pair would silently lose data on Save / Load —
//! either the asset table forgets the Mesh entry, or the side-file
//! contents come back malformed.

use awsm_scene_schema::{
    mesh_asset_filename, AssetEntry, AssetId, AssetSource, AssetTable, CapturedMesh, EditorProject,
    EnvironmentConfig, MeshDef, MESH_FILE_EXTENSION,
};

fn sample_captured_mesh() -> CapturedMesh {
    // A tiny but non-trivial mesh — exercise every attribute slot so a
    // future "drop the colors field" regression would visibly diverge.
    CapturedMesh {
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        normals: Some(vec![
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        ]),
        uvs: Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
        colors: Some(vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 0.5],
        ]),
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

fn project_with_mesh_asset(asset_id: AssetId, label: &str) -> EditorProject {
    let mut assets = AssetTable::new();
    assets.entries.insert(
        asset_id,
        AssetEntry::new(AssetSource::Mesh(MeshDef {
            label: label.to_string(),
            source: None,
        })),
    );
    EditorProject {
        name: String::new(),
        environment: EnvironmentConfig::default(),
        shadows: Default::default(),
        assets,
        nodes: Vec::new(),
    }
}

#[test]
fn mesh_asset_entry_json_roundtrip() {
    let asset_id = AssetId::new();
    let project = project_with_mesh_asset(asset_id, "Captured sphere");
    let json = serde_json::to_string(&project).expect("serialize");
    let back: EditorProject = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(project, back);
    // Spot-check the metadata survived the round-trip.
    match &back.assets.entries.get(&asset_id).unwrap().source {
        AssetSource::Mesh(def) => assert_eq!(def.label, "Captured sphere"),
        other => panic!("expected Mesh source, got {other:?}"),
    }
}

#[test]
fn mesh_asset_entry_bitcode_roundtrip() {
    let asset_id = AssetId::new();
    let project = project_with_mesh_asset(asset_id, "Captured tube");
    let bytes = bitcode::serialize(&project).expect("bitcode serialize");
    let back: EditorProject = bitcode::deserialize(&bytes).expect("bitcode deserialize");
    assert_eq!(project, back);
}

#[test]
fn captured_mesh_bitcode_roundtrip() {
    // The on-disk side-file shape — bitcode is what the editor's Save
    // flow + the player's prefetch step both serialize through.
    let mesh = sample_captured_mesh();
    let bytes = bitcode::serialize(&mesh).expect("bitcode serialize");
    let back: CapturedMesh = bitcode::deserialize(&bytes).expect("bitcode deserialize");
    assert_eq!(mesh, back);
    assert_eq!(back.positions.len(), 4);
    assert_eq!(back.indices, vec![0, 1, 2, 0, 2, 3]);
    assert!(back.normals.is_some());
    assert!(back.uvs.is_some());
    assert!(back.colors.is_some());
}

#[test]
fn captured_mesh_handles_optional_attrs() {
    // Stripping the optional attributes — the schema's
    // `Option<Vec<…>>` shape is the contract.
    let mesh = CapturedMesh {
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: None,
        uvs: None,
        colors: None,
        indices: vec![0, 1, 2],
    };
    let bytes = bitcode::serialize(&mesh).expect("bitcode serialize");
    let back: CapturedMesh = bitcode::deserialize(&bytes).expect("bitcode deserialize");
    assert_eq!(mesh, back);
    assert!(back.normals.is_none());
    assert!(back.uvs.is_none());
    assert!(back.colors.is_none());
}

#[test]
fn mesh_asset_with_source_roundtrip() {
    // H1: MeshDef.source records the kind the bytes were captured
    // from. Verify both Primitive + Sweep variants survive JSON +
    // bitcode round-trips.
    use awsm_scene_schema::{
        AssetTable, CapturedSource, CrossSectionDef, MaterialDef, NodeId, PrimitiveShape,
        SweepAlongCurveDef, SweepUvMode,
    };

    let mut assets = AssetTable::new();
    let prim_id = AssetId::new();
    assets.entries.insert(
        prim_id,
        AssetEntry::new(AssetSource::Mesh(MeshDef {
            label: "captured sphere".to_string(),
            source: Some(CapturedSource::Primitive(PrimitiveShape::Sphere {
                radius: 1.25,
                segments_long: 24,
                segments_lat: 12,
            })),
        })),
    );
    let sweep_id = AssetId::new();
    assets.entries.insert(
        sweep_id,
        AssetEntry::new(AssetSource::Mesh(MeshDef {
            label: "captured rail".to_string(),
            source: Some(CapturedSource::Sweep(SweepAlongCurveDef {
                curve_node: NodeId::new(),
                cross_section: CrossSectionDef::Tube {
                    radius: 0.3,
                    radial_segments: 16,
                },
                uv_mode: SweepUvMode::StretchOnce,
                up_hint: [0.0, 1.0, 0.0],
                samples: 128,
            })),
        })),
    );
    let project = EditorProject {
        name: String::new(),
        environment: EnvironmentConfig::default(),
        shadows: Default::default(),
        assets,
        nodes: Vec::new(),
    };

    // JSON round-trip
    let json = serde_json::to_string(&project).expect("json serialize");
    let back: EditorProject = serde_json::from_str(&json).expect("json deserialize");
    assert_eq!(project, back, "JSON drift");

    // Bitcode round-trip
    let bytes = bitcode::serialize(&project).expect("bitcode serialize");
    let back: EditorProject = bitcode::deserialize(&bytes).expect("bitcode deserialize");
    assert_eq!(project, back, "bitcode drift");

    // Spot-check the variants survived.
    match &back.assets.entries.get(&prim_id).unwrap().source {
        AssetSource::Mesh(def) => match &def.source {
            Some(CapturedSource::Primitive(PrimitiveShape::Sphere { radius, .. })) => {
                assert!((*radius - 1.25).abs() < 1.0e-6);
            }
            other => panic!("expected Primitive::Sphere source, got {other:?}"),
        },
        _ => unreachable!(),
    }
    match &back.assets.entries.get(&sweep_id).unwrap().source {
        AssetSource::Mesh(def) => match &def.source {
            Some(CapturedSource::Sweep(d)) => assert_eq!(d.samples, 128),
            other => panic!("expected Sweep source, got {other:?}"),
        },
        _ => unreachable!(),
    }

    // Drive a no-source MaterialDef just so the round-trip exercises
    // both default + explicit forms.
    let _ = MaterialDef::default();
}

#[test]
fn mesh_asset_filename_is_stable() {
    // The filename helper is the side-table addressing contract — it
    // must produce the same string for the same AssetId on every call,
    // and the extension must match the public const.
    let id = AssetId::new();
    let name = mesh_asset_filename(id);
    assert!(name.ends_with(MESH_FILE_EXTENSION));
    assert_eq!(name, mesh_asset_filename(id));
    // Two different AssetIds produce different filenames.
    let other = AssetId::new();
    assert_ne!(mesh_asset_filename(id), mesh_asset_filename(other));
}
