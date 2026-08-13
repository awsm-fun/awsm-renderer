//! GLB writer round-trip tests. Build a [`GlbScene`], write it, then re-parse —
//! both with the `gltf` reader crate (geometry + material factors) and with raw
//! JSON (extension wiring + referenced-only images).

use awsm_renderer_glb_export::{
    write_glb, ExportLight, ExportMaterial, ExportNode, GlbScene, PbrMaterial, TexRef,
    TexTransform, Trs, UnlitMaterial, AWSM_MATERIALS_NONE,
};
use awsm_renderer_meshgen::box_mesh;
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
fn texture_transform_roundtrip() {
    // KHR_texture_transform on a base-color textureInfo survives write_glb (GAP 3).
    let mut tr = TexRef::new(0);
    tr.transform = Some(TexTransform {
        offset: [0.25, 0.5],
        rotation: 0.0,
        scale: [2.0, 4.0],
        tex_coord: None,
    });
    let scene = GlbScene {
        nodes: vec![ExportNode::new("Cube")
            .with_mesh(box_mesh(Vec3::ONE))
            .with_material(ExportMaterial::Pbr(PbrMaterial {
                base_color_texture: Some(tr),
                ..Default::default()
            }))],
        images: vec![awsm_renderer_glb_export::ExportImage {
            name: "albedo".into(),
            bytes: include_bytes!("fixtures/1x1.png").to_vec(),
            mime: awsm_renderer_glb_export::ImageMime::Png,
        }],
        ..Default::default()
    };
    let glb = write_glb(&scene);
    // Gltf::from_slice parses JSON without decoding images (the 1x1 fixture isn't a
    // real PNG; we only need the textureInfo extension JSON).
    let gltf = gltf::Gltf::from_slice(&glb).expect("re-parse");
    let m = gltf
        .materials()
        .find(|m| m.index().is_some())
        .expect("material");
    let info = m
        .pbr_metallic_roughness()
        .base_color_texture()
        .expect("base color texture");
    let xf = info
        .texture_transform()
        .expect("texture_transform survives");
    assert_eq!(xf.offset(), [0.25, 0.5]);
    assert_eq!(xf.scale(), [2.0, 4.0]);
}

#[test]
fn pbr_scalar_extensions_roundtrip() {
    // KHR_materials_ior + KHR_materials_emissive_strength survive write_glb as raw JSON
    // in the material's `extensions.others` map (GAP 3). Re-parse via the gltf reader's
    // typed accessors (features on) to confirm the values round-trip.
    let scene = cube_scene_with(ExportMaterial::Pbr(PbrMaterial {
        ior: Some(1.4),
        emissive_strength: Some(3.0),
        ..Default::default()
    }));
    let glb = write_glb(&scene);
    let (doc, _b, _i) = gltf::import_slice(&glb).expect("re-parse");
    let mat = doc
        .materials()
        .find(|m| m.index().is_some())
        .expect("a non-default material");
    assert_eq!(mat.ior(), Some(1.4), "ior round-trips");
    assert_eq!(
        mat.emissive_strength(),
        Some(3.0),
        "emissive_strength round-trips"
    );
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
        images: vec![awsm_renderer_glb_export::ExportImage {
            name: "albedo".into(),
            bytes: png,
            mime: awsm_renderer_glb_export::ImageMime::Png,
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
fn extract_texture_images_roundtrips_encoded_bytes() {
    // The editor captures imported textures for persistence via
    // extract_texture_images_from_bytes — assert it returns the ORIGINAL encoded
    // bytes (not re-encoded), keyed by glTF texture index, for a referenced texture.
    let png = include_bytes!("fixtures/1x1.png").to_vec();
    let scene = GlbScene {
        nodes: vec![ExportNode::new("Cube")
            .with_mesh(box_mesh(Vec3::ONE))
            .with_material(ExportMaterial::Pbr(PbrMaterial {
                base_color_texture: Some(TexRef::new(0)),
                ..Default::default()
            }))],
        images: vec![awsm_renderer_glb_export::ExportImage {
            name: "albedo".into(),
            bytes: png.clone(),
            mime: awsm_renderer_glb_export::ImageMime::Png,
        }],
        ..Default::default()
    };
    let glb = write_glb(&scene);
    let images = awsm_renderer_glb_export::extract_texture_images_from_bytes(&glb);
    // One texture, at index 0, with byte-identical PNG bytes + png ext.
    assert_eq!(images.len(), 1);
    let img = images.get(&0).expect("texture 0");
    assert_eq!(img.bytes, png, "encoded bytes must round-trip exactly");
    assert_eq!(img.mime, awsm_renderer_glb_export::ImageMime::Png);
    assert_eq!(img.mime.ext(), "png");
}

#[test]
fn animation_channel_roundtrips() {
    use awsm_renderer_glb_export::{AnimInterp, AnimPath, ExportAnimChannel, ExportAnimation};
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
fn multi_uv_sets_roundtrip() {
    use awsm_renderer_glb_export::MeshData;
    // A triangle with TWO UV sets (TEXCOORD_0 + TEXCOORD_1) — both must survive
    // write_glb so multi-UV meshes (e.g. an AO map on set 1) round-trip.
    let tri = MeshData {
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
        uvs: vec![
            vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            vec![[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]],
        ],
        colors: None,
        indices: vec![0, 1, 2],
    };
    let scene = GlbScene {
        nodes: vec![ExportNode::new("Tri").with_mesh(tri)],
        ..Default::default()
    };
    let glb = write_glb(&scene);
    let (doc, buffers, _i) = gltf::import_slice(&glb).expect("re-parse");
    let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
    let reader = prim.reader(|b| Some(&buffers[b.index()]));
    let uv0: Vec<[f32; 2]> = reader
        .read_tex_coords(0)
        .expect("TEXCOORD_0")
        .into_f32()
        .collect();
    let uv1: Vec<[f32; 2]> = reader
        .read_tex_coords(1)
        .expect("TEXCOORD_1")
        .into_f32()
        .collect();
    assert_eq!(uv0[1], [1.0, 0.0]);
    assert_eq!(uv1[2], [0.5, 0.6]);
}

#[test]
fn extract_node_mesh_folds_uv_sets() {
    use awsm_renderer_glb_export::{extract_node_mesh_from_bytes, MeshData};
    // A 2-UV mesh, re-extracted via the editor's node path, folds BOTH sets into
    // mesh.uvs (no separate uvs1 channel) — GPU multi-UV step 2.
    let tri = MeshData {
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
        uvs: vec![
            vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            vec![[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]],
        ],
        colors: None,
        indices: vec![0, 1, 2],
    };
    let scene = GlbScene {
        nodes: vec![ExportNode::new("Tri").with_mesh(tri)],
        ..Default::default()
    };
    let bytes = write_glb(&scene);
    let got = extract_node_mesh_from_bytes(&bytes, 0, None).expect("extract node mesh");
    assert_eq!(got.uvs.len(), 2, "both UV sets folded into mesh.uvs");
    assert_eq!(got.uvs[0][1], [1.0, 0.0]);
    assert_eq!(got.uvs[1][2], [0.5, 0.6]);
}

#[test]
fn skinned_morph_mesh_roundtrips() {
    use awsm_renderer_glb_export::{ExportSkin, MeshData, MorphTarget};

    // A triangle skinned to a 2-joint skeleton, with one morph target.
    let tri = MeshData {
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
        uvs: vec![],
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

#[test]
fn eight_influences_round_trip_through_two_joint_sets() {
    // A MuJoCo trilinear flex needs eight influences — one per corner of its
    // cage — so the writer must emit JOINTS_1/WEIGHTS_1 and the extractor must
    // read them back. Four would silently drop half the cage and deform the
    // mesh toward whichever corners happened to survive.
    use awsm_renderer_glb_export::{ExportNode, ExportSkin, GlbScene, MeshData, SkinInfluenceSet};

    let verts = 3;
    let mesh = MeshData {
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        indices: vec![0, 1, 2],
        ..Default::default()
    };

    let mut node = ExportNode {
        name: "flex".into(),
        mesh: Some(mesh),
        skin: Some(0),
        joints: Some(vec![[0, 1, 2, 3]; verts]),
        // All EIGHT influences partition unity between them — the trilinear
        // basis at a cell centre, which is the worst case for truncating to 4.
        weights: Some(vec![[0.125, 0.125, 0.125, 0.125]; verts]),
        ..Default::default()
    };
    node.extra_influence_sets = vec![SkinInfluenceSet {
        joints: vec![[4, 5, 6, 7]; verts],
        weights: vec![[0.125, 0.125, 0.125, 0.125]; verts],
    }];

    let joint_nodes: Vec<ExportNode> = (0..8)
        .map(|i| ExportNode {
            name: format!("cage{i}"),
            ..Default::default()
        })
        .collect();
    let mut scene = GlbScene {
        nodes: std::iter::once(node).chain(joint_nodes).collect(),
        ..Default::default()
    };
    scene.skins = vec![ExportSkin {
        joints: (1..9).collect(),
        inverse_bind_matrices: vec![],
        ..Default::default()
    }];

    let glb = awsm_renderer_glb_export::write_glb(&scene);
    let (doc, buffers, _) = gltf::import_slice(&glb).expect("a loadable glTF");
    let raw: Vec<Vec<u8>> = buffers.iter().map(|b| b.0.clone()).collect();
    let node_index = doc
        .nodes()
        .find(|n| n.name() == Some("flex"))
        .expect("flex node")
        .index() as u32;
    let ex =
        awsm_renderer_glb_export::extract_node_mesh(&doc, &raw, node_index, None).expect("extract");
    let skin = ex.skin.expect("skin");

    assert_eq!(skin.set_count(), 2, "both influence sets must survive");
    assert_eq!(skin.joints[0], [0, 1, 2, 3]);
    assert_eq!(skin.extra_sets[0].joints[0], [4, 5, 6, 7]);
    // Weights across BOTH sets partition unity — the invariant that makes the
    // eight-corner reconstruction exact.
    let total: f32 =
        skin.weights[0].iter().sum::<f32>() + skin.extra_sets[0].weights[0].iter().sum::<f32>();
    assert!(
        (total - 1.0).abs() < 1e-6,
        "weights must sum to 1, got {total}"
    );
    assert_eq!(skin.packed_index_weights().len(), verts * 2 * 8 * 4);

    // The CLEAN RE-EXPORT is what the editor's materialiser actually decodes, so
    // it has to carry both sets too. Dropping them here is invisible in the
    // source GLB and collapses the mesh at runtime.
    let clean = awsm_renderer_glb_export::reexport_clean(&glb).expect("clean re-export");
    let flex_node = clean
        .nodes
        .iter()
        .find(|n| n.name == "flex")
        .expect("flex node survives the clean re-export");
    assert_eq!(
        flex_node.extra_influence_sets.len(),
        1,
        "the second influence set must survive reexport_clean"
    );
    assert_eq!(flex_node.extra_influence_sets[0].joints[0], [4, 5, 6, 7]);

    let clean_glb = awsm_renderer_glb_export::write_glb(&clean);
    let (cdoc, cbuf, _) = gltf::import_slice(&clean_glb).expect("loadable clean glTF");
    let craw: Vec<Vec<u8>> = cbuf.iter().map(|b| b.0.clone()).collect();
    let cindex = cdoc
        .nodes()
        .find(|n| n.name() == Some("flex"))
        .expect("flex node")
        .index() as u32;
    let cex = awsm_renderer_glb_export::extract_node_mesh(&cdoc, &craw, cindex, None)
        .expect("extract from clean");
    assert_eq!(
        cex.skin.expect("clean skin").set_count(),
        2,
        "a full write -> clean re-export -> write -> read cycle must keep both sets"
    );
}
