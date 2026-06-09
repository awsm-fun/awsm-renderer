//! Player-bundle integration test: assemble a bundle from the same kinds of
//! pieces the editor produces (a real baked `scene.glb`, custom-material
//! side-files, a referenced texture, an environment descriptor) and write it to a
//! throwaway directory — then assert the on-disk layout is the complete, loadable
//! bundle. This is the "editor project → bundle dir" path as far as it can be
//! exercised without a GPU/browser: the scene bytes come from the real
//! `write_glb`, and everything downstream (layout + manifest + dir write) is what
//! the player consumes.

use awsm_glb_export::{
    assemble_bundle, write_glb, BundleInputs, ExportMaterial, ExportNode, GlbScene, PbrMaterial,
};
use awsm_meshgen::box_mesh;
use glam::Vec3;

/// A real one-mesh `scene.glb` (the geometry+material half the GPU exporter would
/// produce), so the test exercises a genuine bundle payload, not a stub.
fn scene_glb() -> Vec<u8> {
    let node = ExportNode::new("Box")
        .with_mesh(box_mesh(Vec3::splat(1.0)))
        .with_material(ExportMaterial::Pbr(PbrMaterial {
            name: "Mat".into(),
            base_color: [0.2, 0.6, 0.9, 1.0],
            ..Default::default()
        }));
    write_glb(&GlbScene {
        nodes: vec![node],
        ..Default::default()
    })
}

#[test]
fn assembles_and_writes_a_complete_bundle_dir() {
    let glb = scene_glb();
    let inputs = BundleInputs {
        scene_glb: glb.clone(),
        materials: vec![
            (
                "materials/mat-1/material.wgsl".to_string(),
                "// custom wgsl".to_string(),
            ),
            (
                "materials/mat-1/material.toml".to_string(),
                "name = \"mat-1\"\n".to_string(),
            ),
        ],
        textures: vec![("checker.png".to_string(), b"\x89PNG\r\n\x1a\nDATA".to_vec())],
        env_json: Some("{\"skybox\":\"sky.hdr\"}".to_string()),
    };

    let bundle = assemble_bundle("my-game", inputs);
    let dir = tempfile::tempdir().expect("tempdir");
    bundle.write_to_dir(dir.path()).expect("write bundle");
    let root = dir.path();

    // scene.glb: present + a valid GLB the gltf reader re-parses with geometry.
    let glb_on_disk = std::fs::read(root.join("scene.glb")).expect("scene.glb exists");
    assert_eq!(glb_on_disk, glb, "scene.glb written byte-for-byte");
    let (doc, buffers, _images) = gltf::import_slice(&glb_on_disk).expect("scene.glb re-parses");
    let prim = doc
        .meshes()
        .next()
        .and_then(|m| m.primitives().next())
        .expect("a primitive");
    let reader = prim.reader(|b| Some(&buffers[b.index()]));
    assert!(
        reader.read_positions().expect("positions").count() > 0,
        "scene.glb carries geometry"
    );

    // Material side-files at their exact relative paths.
    assert_eq!(
        std::fs::read_to_string(root.join("materials/mat-1/material.wgsl")).unwrap(),
        "// custom wgsl"
    );
    assert!(root.join("materials/mat-1/material.toml").exists());

    // Texture copied under textures/.
    assert!(root.join("textures/checker.png").exists());

    // Environment sidecar.
    assert!(root.join("env.json").exists());

    // bundle.json indexes everything so the player loads from one entry point.
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("bundle.json")).unwrap()).unwrap();
    assert_eq!(manifest["name"], "my-game");
    assert_eq!(manifest["scene"], "scene.glb");
    assert_eq!(manifest["materials"].as_array().unwrap().len(), 2);
    assert_eq!(
        manifest["textures"],
        serde_json::json!(["textures/checker.png"])
    );
    assert_eq!(manifest["env"], "env.json");
}

#[test]
fn omits_env_when_absent_and_rejects_unsafe_paths() {
    // No env → no env.json, and the manifest's env is null.
    let bundle = assemble_bundle(
        "g",
        BundleInputs {
            scene_glb: scene_glb(),
            ..Default::default()
        },
    );
    let dir = tempfile::tempdir().unwrap();
    bundle.write_to_dir(dir.path()).unwrap();
    assert!(!dir.path().join("env.json").exists());
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("bundle.json")).unwrap()).unwrap();
    assert!(manifest["env"].is_null());
    assert!(manifest["textures"].as_array().unwrap().is_empty());

    // A crafted material path must not escape the bundle root.
    let escape = assemble_bundle(
        "g",
        BundleInputs {
            scene_glb: vec![1, 2, 3],
            materials: vec![("../escape.wgsl".to_string(), "x".to_string())],
            ..Default::default()
        },
    );
    let dir2 = tempfile::tempdir().unwrap();
    assert!(
        escape.write_to_dir(dir2.path()).is_err(),
        "path traversal rejected"
    );
}
