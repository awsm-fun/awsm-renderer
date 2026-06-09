//! GLB writer round-trip tests. Build a [`GlbScene`], write it, then re-parse —
//! both with the `gltf` reader crate (geometry + material factors) and with raw
//! JSON (extension wiring + referenced-only images).

use awsm_glb_export::{
    write_glb, ExportLight, ExportMaterial, ExportNode, GlbScene, PbrMaterial, TexRef, Trs,
    UnlitMaterial, AWSM_MATERIALS_NONE,
};
use awsm_meshgen::box_mesh;
use glam::Vec3;
use serde_json::Value;

/// Extract + parse the GLB JSON chunk as a `serde_json::Value`.
fn glb_json(bytes: &[u8]) -> Value {
    assert_eq!(&bytes[0..4], b"glTF", "GLB magic");
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    assert_eq!(&bytes[16..20], b"JSON", "first chunk is JSON");
    let json = &bytes[20..20 + json_len];
    serde_json::from_slice(json).expect("valid JSON chunk")
}

fn cube_scene_with(material: ExportMaterial) -> GlbScene {
    let mut node = ExportNode::new("Cube")
        .with_mesh(box_mesh(Vec3::splat(2.0)))
        .with_material(material);
    node.transform = Trs::IDENTITY;
    GlbScene {
        nodes: vec![node],
        ..Default::default()
    }
}

#[test]
fn cube_roundtrip_pbr() {
    let scene = cube_scene_with(ExportMaterial::Pbr(PbrMaterial {
        name: "Red".into(),
        base_color: [1.0, 0.0, 0.0, 1.0],
        metallic: 0.25,
        roughness: 0.75,
        ..Default::default()
    }));
    let src = box_mesh(Vec3::splat(2.0));
    let glb = write_glb(&scene);

    // The gltf reader validates the whole document (incl. POSITION min/max).
    let (doc, buffers, images) = gltf::import_slice(&glb).expect("re-parse GLB");
    assert_eq!(
        images.len(),
        0,
        "no textures referenced ⇒ no images embedded"
    );

    let mesh = doc.meshes().next().expect("one mesh");
    let prim = mesh.primitives().next().expect("one primitive");
    let reader = prim.reader(|b| Some(&buffers[b.index()]));

    let positions: Vec<_> = reader.read_positions().expect("positions").collect();
    assert_eq!(positions.len(), src.positions.len());
    let indices: Vec<u32> = reader.read_indices().expect("indices").into_u32().collect();
    assert_eq!(indices.len(), src.indices.len());

    let pbr = prim.material().pbr_metallic_roughness();
    assert_eq!(pbr.base_color_factor(), [1.0, 0.0, 0.0, 1.0]);
    assert!((pbr.metallic_factor() - 0.25).abs() < 1e-6);
    assert!((pbr.roughness_factor() - 0.75).abs() < 1e-6);
}

#[test]
fn cube_roundtrip_unlit() {
    let scene = cube_scene_with(ExportMaterial::Unlit(UnlitMaterial {
        name: "Flat".into(),
        base_color: [0.2, 0.4, 0.6, 1.0],
        ..Default::default()
    }));
    let glb = write_glb(&scene);

    // gltf reader: base color survives.
    let (doc, _b, _i) = gltf::import_slice(&glb).expect("re-parse GLB");
    let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
    assert_eq!(
        prim.material().pbr_metallic_roughness().base_color_factor(),
        [0.2, 0.4, 0.6, 1.0]
    );

    // Raw JSON: the unlit extension is declared + present on the material.
    let v = glb_json(&glb);
    let used = v["extensionsUsed"].as_array().expect("extensionsUsed");
    assert!(used.iter().any(|e| e == "KHR_materials_unlit"));
    assert!(v["materials"][0]["extensions"]["KHR_materials_unlit"].is_object());
}

#[test]
fn cube_roundtrip_materials_none() {
    let scene = cube_scene_with(ExportMaterial::None {
        id: Some("mat-custom-1".into()),
    });
    let glb = write_glb(&scene);
    let v = glb_json(&glb);

    // No embedded material at all.
    assert!(
        v.get("materials").is_none() || v["materials"].as_array().unwrap().is_empty(),
        "non-PBR ⇒ no embedded glTF material"
    );
    // The primitive carries the AWSM_materials_none extension with the id.
    let prim = &v["meshes"][0]["primitives"][0];
    assert!(
        prim.get("material").is_none(),
        "primitive has no material index"
    );
    let ext = &prim["extensions"][AWSM_MATERIALS_NONE];
    assert!(ext.is_object(), "AWSM_materials_none present on primitive");
    assert_eq!(ext["id"], "mat-custom-1");

    let used = v["extensionsUsed"].as_array().expect("extensionsUsed");
    assert!(used.iter().any(|e| e == AWSM_MATERIALS_NONE));
}

#[test]
fn lightweighting_drops_unreferenced_textures() {
    // A PBR material that references NO textures ⇒ the export embeds zero images,
    // regardless of what the original import carried. This is the referenced-only
    // rule that makes "slimming" fall out of reassigning a lighter material.
    let scene = cube_scene_with(ExportMaterial::Pbr(PbrMaterial::default()));
    let glb = write_glb(&scene);
    let (_doc, _buffers, images) = gltf::import_slice(&glb).unwrap();
    assert_eq!(images.len(), 0);
    let v = glb_json(&glb);
    assert!(v.get("images").is_none() || v["images"].as_array().unwrap().is_empty());
}

#[test]
fn referenced_texture_is_embedded() {
    // A 1x1 PNG (smallest valid-ish payload for the writer; the reader only needs
    // the bytes present + a mimeType — it does not decode here).
    let png = include_bytes!("fixtures/1x1.png").to_vec();
    let scene = GlbScene {
        nodes: vec![ExportNode::new("Cube")
            .with_mesh(box_mesh(Vec3::ONE))
            .with_material(ExportMaterial::Pbr(PbrMaterial {
                base_color_texture: Some(TexRef::new(0)),
                ..Default::default()
            }))],
        images: vec![awsm_glb_export::ExportImage {
            name: "albedo".into(),
            bytes: png,
            mime: awsm_glb_export::ImageMime::Png,
        }],
        ..Default::default()
    };
    let glb = write_glb(&scene);
    let v = glb_json(&glb);
    assert_eq!(v["images"].as_array().expect("images").len(), 1);
    assert_eq!(v["textures"].as_array().expect("textures").len(), 1);
    assert_eq!(v["images"][0]["mimeType"], "image/png");
    // base color texture points at texture 0.
    assert_eq!(
        v["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["index"],
        0
    );
}

#[test]
fn animation_channel_roundtrips() {
    use awsm_glb_export::{AnimInterp, AnimPath, ExportAnimChannel, ExportAnimation};
    // One node + a rotation track (two quaternion keyframes at t=0,1).
    let scene = GlbScene {
        nodes: vec![ExportNode::new("Spinner")],
        animations: vec![ExportAnimation {
            name: "spin".into(),
            channels: vec![ExportAnimChannel {
                node_index: 0,
                path: AnimPath::Rotation,
                interpolation: AnimInterp::Linear,
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0],
            }],
        }],
        ..Default::default()
    };
    let glb = write_glb(&scene);
    let (doc, buffers, _i) = gltf::import_slice(&glb).expect("re-parse GLB");
    let anim = doc.animations().next().expect("one animation");
    let ch = anim.channels().next().expect("one channel");
    assert_eq!(ch.target().property(), gltf::animation::Property::Rotation);
    assert_eq!(ch.target().node().index(), 0);
    let reader = ch.reader(|b| Some(&buffers[b.index()]));
    let inputs: Vec<f32> = reader.read_inputs().expect("inputs").collect();
    assert_eq!(inputs, vec![0.0, 1.0]);
    match reader.read_outputs().expect("outputs") {
        gltf::animation::util::ReadOutputs::Rotations(rot) => {
            assert_eq!(rot.into_f32().count(), 2);
        }
        _ => panic!("expected rotation outputs"),
    }
}

#[test]
fn skinned_morph_mesh_roundtrips() {
    use awsm_glb_export::{ExportSkin, MeshData, MorphTarget};

    // A triangle skinned to a 2-joint skeleton, with one morph target.
    let tri = MeshData {
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
        uvs: None,
        colors: None,
        indices: vec![0, 1, 2],
    };
    let scene = GlbScene {
        // Armature(0) → J0(1), J1(2); skinned Mesh(3).
        nodes: vec![
            ExportNode {
                name: "Armature".into(),
                children: vec![ExportNode::new("J0"), ExportNode::new("J1")],
                ..Default::default()
            },
            ExportNode {
                name: "Mesh".into(),
                mesh: Some(tri),
                material: Some(ExportMaterial::None { id: None }),
                skin: Some(0),
                joints: Some(vec![[0, 1, 0, 0]; 3]),
                weights: Some(vec![[0.5, 0.5, 0.0, 0.0]; 3]),
                morph_targets: vec![MorphTarget {
                    name: Some("bulge".into()),
                    positions: vec![[0.0, 0.1, 0.0]; 3],
                    normals: None,
                }],
                morph_weights: vec![0.0],
                ..Default::default()
            },
        ],
        skins: vec![ExportSkin {
            joints: vec![1, 2],
            inverse_bind_matrices: vec![
                // identity ×2 (column-major)
                [
                    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
                ],
                [
                    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
                ],
            ],
            skeleton: Some(0),
        }],
        ..Default::default()
    };

    let glb = write_glb(&scene);
    // import_slice fully validates accessors (incl. JOINTS/WEIGHTS/IBM/targets).
    let (doc, buffers, _i) = gltf::import_slice(&glb).expect("re-parse skinned GLB");

    // Skin: 2 joints + inverse-bind matrices.
    let skin = doc.skins().next().expect("one skin");
    assert_eq!(skin.joints().count(), 2, "two joints");
    assert!(
        skin.inverse_bind_matrices().is_some(),
        "inverse-bind accessor present"
    );
    assert_eq!(skin.skeleton().map(|n| n.index()), Some(0));

    // The skinned node references the skin.
    let mesh_node = doc.nodes().find(|n| n.name() == Some("Mesh")).unwrap();
    assert_eq!(mesh_node.skin().map(|s| s.index()), Some(0));

    // Per-vertex JOINTS_0 / WEIGHTS_0 read back.
    let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
    let reader = prim.reader(|b| Some(&buffers[b.index()]));
    let joints: Vec<_> = reader
        .read_joints(0)
        .expect("JOINTS_0")
        .into_u16()
        .collect();
    assert_eq!(joints.len(), 3);
    assert_eq!(joints[0], [0, 1, 0, 0]);
    let weights: Vec<_> = reader
        .read_weights(0)
        .expect("WEIGHTS_0")
        .into_f32()
        .collect();
    assert_eq!(weights.len(), 3);
    assert!((weights[0][0] - 0.5).abs() < 1e-6);

    // Morph target present + its position deltas read back.
    assert_eq!(prim.morph_targets().count(), 1, "one morph target");
    assert!(
        prim.morph_targets().next().unwrap().positions().is_some(),
        "morph positions accessor present"
    );
    let mut mt_reader = reader.read_morph_targets();
    let (pos, _normals, _tangents) = mt_reader.next().expect("one morph target reader");
    let deltas: Vec<_> = pos.expect("morph positions").collect();
    assert_eq!(deltas.len(), 3);
    assert!((deltas[0][1] - 0.1).abs() < 1e-6, "y-delta 0.1");
}

#[test]
fn scene_complete_light_node() {
    // Phase 6 reuse smoke test: a light-only node lowers to KHR_lights_punctual
    // even with no geometry (empty BIN ⇒ no buffer, still valid JSON).
    let scene = GlbScene {
        nodes: vec![ExportNode {
            name: "Sun".into(),
            light: Some(ExportLight::Directional {
                color: [1.0, 0.95, 0.8],
                intensity: 4.0,
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let glb = write_glb(&scene);
    let v = glb_json(&glb);
    let used = v["extensionsUsed"].as_array().unwrap();
    assert!(used.iter().any(|e| e == "KHR_lights_punctual"));
    assert!(v["extensions"]["KHR_lights_punctual"]["lights"][0].is_object());
    assert!(v["nodes"][0]["extensions"]["KHR_lights_punctual"]["light"].is_number());
    // No mesh ⇒ no BIN chunk.
    assert!(v.get("buffers").is_none() || v["buffers"].as_array().unwrap().is_empty());
}
