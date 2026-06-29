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

use awsm_renderer_editor_protocol::{
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
        // 2nd UV set (TEXCOORD_1) — distinct from set 0 so a "drop uvs1" regression diverges.
        uvs1: Some(vec![[0.0, 1.0], [0.5, 0.5], [1.0, 0.0], [0.25, 0.75]]),
        colors: Some(vec![
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 0.5],
        ]),
        tangents: None,
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
            editable: false,
            stack: awsm_renderer_editor_protocol::ModifierStack {
                base: awsm_renderer_editor_protocol::MeshBase::Captured(
                    awsm_renderer_editor_protocol::MeshRef(asset_id),
                ),
                modifiers: vec![],
            },
            overrides: Default::default(),
        })),
    );
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

/// P2-D: a NON-empty modifier stack (the audit flagged this as untested) must
/// round-trip exactly through both project.json (serde-JSON) and the per-game
/// bitcode artifact — so an edited mesh's recipe reloads identically.
#[test]
fn mesh_modifier_stack_roundtrips() {
    use awsm_renderer_meshgen::recipe::{Axis, Modifier};
    let asset_id = AssetId::new();
    let mut project = project_with_mesh_asset(asset_id, "modded");
    if let AssetSource::Mesh(def) = &mut project.assets.entries.get_mut(&asset_id).unwrap().source {
        def.stack.modifiers = vec![
            Modifier::Twist {
                axis: Axis::Y,
                turns: 0.75,
            },
            Modifier::Inflate { amount: 0.2 },
            Modifier::Array {
                count: 4,
                offset: [1.0, 0.0, 0.5],
            },
            Modifier::Displace {
                expr: "sin(x*3.0)".to_string(),
            },
        ];
    }
    let json = serde_json::to_string(&project).expect("serialize");
    assert_eq!(
        project,
        serde_json::from_str::<EditorProject>(&json).expect("deserialize"),
        "modifier stack json roundtrip"
    );
    let bytes = bitcode::serialize(&project).expect("bitcode serialize");
    assert_eq!(
        project,
        bitcode::deserialize::<EditorProject>(&bytes).expect("bitcode deserialize"),
        "modifier stack bitcode roundtrip"
    );
    match &project.assets.entries.get(&asset_id).unwrap().source {
        AssetSource::Mesh(def) => assert_eq!(def.stack.modifiers.len(), 4),
        other => panic!("expected Mesh, got {other:?}"),
    }
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
    assert_eq!(
        back.uvs1,
        Some(vec![[0.0, 1.0], [0.5, 0.5], [1.0, 0.0], [0.25, 0.75]])
    );
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
        uvs1: None,
        colors: None,
        tangents: None,
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
    use awsm_renderer_editor_protocol::{
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
            editable: false,
            stack: awsm_renderer_editor_protocol::ModifierStack {
                base: awsm_renderer_editor_protocol::MeshBase::Primitive(PrimitiveShape::Sphere {
                    radius: 1.25,
                    segments_long: 24,
                    segments_lat: 12,
                }),
                modifiers: vec![],
            },
            overrides: Default::default(),
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
            editable: false,
            stack: awsm_renderer_editor_protocol::ModifierStack {
                base: awsm_renderer_editor_protocol::MeshBase::Sweep(SweepAlongCurveDef {
                    curve_node: NodeId::new(),
                    cross_section: CrossSectionDef::Tube {
                        radius: 0.3,
                        radial_segments: 16,
                    },
                    uv_mode: SweepUvMode::StretchOnce,
                    up_hint: [0.0, 1.0, 0.0],
                    samples: 128,
                }),
                modifiers: vec![],
            },
            overrides: Default::default(),
        })),
    );
    let project = EditorProject {
        name: String::new(),
        environment: EnvironmentConfig::default(),
        shadows: Default::default(),
        assets,
        custom_materials: Vec::new(),
        editor_materials: Vec::new(),
        custom_animations: Vec::new(),
        editor_animations: Vec::new(),
        anim_mixer: Default::default(),
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
fn vertex_overrides_uvs_roundtrip() {
    // The UV override channel is what `SetVertexUvs` writes (the closing gap in
    // the per-vertex authoring family). A "drop the uvs map" regression would
    // silently lose authored strip parameterizations — exercise JSON + bitcode.
    use awsm_renderer_editor_protocol::VertexOverrides;
    let mut ov = VertexOverrides::default();
    ov.uvs.insert(0, [0.0, 0.0]);
    ov.uvs.insert(1, [1.0, 0.0]);
    ov.uvs.insert(7, [0.5, 0.25]);
    ov.colors.insert(3, [1.0, 0.0, 0.0, 1.0]);

    let json = serde_json::to_string(&ov).expect("json serialize");
    let back: VertexOverrides = serde_json::from_str(&json).expect("json deserialize");
    assert_eq!(ov, back, "JSON drift");
    assert_eq!(back.uvs.get(&7), Some(&[0.5, 0.25]));

    let bytes = bitcode::serialize(&ov).expect("bitcode serialize");
    let back: VertexOverrides = bitcode::deserialize(&bytes).expect("bitcode deserialize");
    assert_eq!(ov, back, "bitcode drift");
    assert!(!back.is_empty());
}

#[test]
fn set_vertex_uvs_command_json_roundtrip() {
    // The new write verb must survive the dispatch wire (serde-tagged JSON).
    use awsm_renderer_editor_protocol::EditorCommand;
    let cmd = EditorCommand::SetVertexUvs {
        mesh: AssetId::new(),
        indices: vec![0, 1, 2],
        uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
        selection: None,
    };
    let json = serde_json::to_string(&cmd).expect("json serialize");
    // Tagged with `cmd` like every other EditorCommand.
    assert!(
        json.contains("\"cmd\":\"set_vertex_uvs\""),
        "tag missing: {json}"
    );
    let back: EditorCommand = serde_json::from_str(&json).expect("json deserialize");
    match back {
        EditorCommand::SetVertexUvs { indices, uvs, .. } => {
            assert_eq!(indices, vec![0, 1, 2]);
            assert_eq!(uvs, vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]);
        }
        other => panic!("expected SetVertexUvs, got {other:?}"),
    }
}

#[test]
fn captured_mesh_validate_rejects_empty_and_degenerate() {
    // The set_mesh_data guard (Item 3): empty/degenerate geometry must be
    // rejected unless allow_empty, and structural invariants always hold.
    let good = sample_captured_mesh();
    assert!(good.validate(false).is_ok(), "valid mesh should pass");

    // Empty wipe — rejected by default, allowed with allow_empty.
    let empty = CapturedMesh {
        positions: vec![],
        normals: None,
        uvs: None,
        uvs1: None,
        colors: None,
        tangents: None,
        indices: vec![],
    };
    assert!(empty.validate(false).is_err(), "empty should be rejected");
    assert!(
        empty.validate(true).is_ok(),
        "empty allowed with allow_empty"
    );

    // Indices not a multiple of 3.
    let mut bad = sample_captured_mesh();
    bad.indices = vec![0, 1];
    assert!(
        bad.validate(false).is_err(),
        "non-triangle indices rejected"
    );
    assert!(
        bad.validate(true).is_err(),
        "allow_empty does NOT waive structural checks"
    );

    // Index out of range for positions (4 verts → max valid index 3).
    let mut oor = sample_captured_mesh();
    oor.indices = vec![0, 1, 99];
    assert!(oor.validate(false).is_err(), "out-of-range index rejected");

    // Misaligned optional channel (normals shorter than positions).
    let mut mis = sample_captured_mesh();
    mis.normals = Some(vec![[0.0, 0.0, 1.0]]);
    assert!(mis.validate(false).is_err(), "misaligned normals rejected");
}

#[test]
fn set_mesh_data_command_allow_empty_defaults_false() {
    // allow_empty is #[serde(default)] — omitting it deserializes to false so the
    // guard is on by default; older project JSON without the field round-trips.
    use awsm_renderer_editor_protocol::EditorCommand;
    let json = format!(
        "{{\"cmd\":\"set_mesh_data\",\"mesh\":\"{}\",\"data\":{{\"positions\":[[0,0,0],[1,0,0],[0,1,0]],\"normals\":null,\"uvs\":null,\"colors\":null,\"indices\":[0,1,2]}}}}",
        AssetId::new()
    );
    let cmd: EditorCommand = serde_json::from_str(&json).expect("deserialize");
    match cmd {
        EditorCommand::SetMeshData { allow_empty, .. } => assert!(!allow_empty),
        other => panic!("expected SetMeshData, got {other:?}"),
    }
}

#[test]
fn separate_mesh_command_json_roundtrip() {
    use awsm_renderer_editor_protocol::{EditorCommand, NodeId};
    let cmd = EditorCommand::SeparateMesh {
        node: NodeId::new(),
        indices: vec![0, 1, 2, 3],
        selection: None,
        new_node: Some(NodeId::new()),
        keep_remainder: true,
    };
    let json = serde_json::to_string(&cmd).expect("serialize");
    assert!(
        json.contains("\"cmd\":\"separate_mesh\""),
        "tag missing: {json}"
    );
    let back: EditorCommand = serde_json::from_str(&json).expect("deserialize");
    match back {
        EditorCommand::SeparateMesh {
            indices,
            keep_remainder,
            new_node,
            ..
        } => {
            assert_eq!(indices, vec![0, 1, 2, 3]);
            assert!(keep_remainder);
            assert!(new_node.is_some());
        }
        other => panic!("expected SeparateMesh, got {other:?}"),
    }

    // Minimal form: indices/selection/new_node/keep_remainder all default.
    let json = format!(
        "{{\"cmd\":\"separate_mesh\",\"node\":\"{}\"}}",
        NodeId::new()
    );
    let back: EditorCommand = serde_json::from_str(&json).expect("deserialize minimal");
    match back {
        EditorCommand::SeparateMesh {
            keep_remainder,
            new_node,
            ..
        } => {
            assert!(!keep_remainder);
            assert!(new_node.is_none());
        }
        other => panic!("expected SeparateMesh, got {other:?}"),
    }
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
