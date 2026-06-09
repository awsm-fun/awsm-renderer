//! The runtime **project directory**: a `scene.toml` + an `assets/` directory —
//! the lean, canonical on-disk form the player loads. The editor's bake produces
//! one; this module owns the `scene.toml` (de)serialization + the directory
//! layout convention.
//!
//! Layout:
//! ```text
//! <bundle>/
//!   scene.toml                 # the runtime Scene (this module)
//!   assets/
//!     <id>.glb                  # baked mesh geometry + skin-rig + morph targets
//!     <hash>.png / .ktx2 / …    # textures our materials reference
//!     materials/<name>/…        # custom-material wgsl + layout side files
//! ```

use crate::scene::Scene;

/// The conventional scene-document filename inside a bundle directory.
pub const SCENE_FILE: &str = "scene.toml";

/// The conventional assets subdirectory.
pub const ASSETS_DIR: &str = "assets";

/// Serialize a [`Scene`] to its `scene.toml` text.
pub fn scene_to_toml(scene: &Scene) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(scene)
}

/// Parse `scene.toml` text into a [`Scene`].
pub fn scene_from_toml(text: &str) -> Result<Scene, toml::de::Error> {
    toml::from_str(text)
}

/// One file in a baked bundle: a bundle-relative path + its bytes. The editor's
/// bake emits `scene.toml` plus the `assets/` files as a `Vec<BundleFile>`, which
/// the caller writes (native fs, or browser File System Access / a download).
#[derive(Clone, Debug, PartialEq)]
pub struct BundleFile {
    /// Bundle-relative path, e.g. `scene.toml` or `assets/<id>.glb`.
    pub path: String,
    pub bytes: Vec<u8>,
}

impl BundleFile {
    pub fn new(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            bytes,
        }
    }

    /// An `assets/<leaf>` file.
    pub fn asset(leaf: impl AsRef<str>, bytes: Vec<u8>) -> Self {
        Self::new(format!("{ASSETS_DIR}/{}", leaf.as_ref()), bytes)
    }
}

/// Assemble a bundle's `scene.toml` file plus the caller-provided asset files
/// (glbs / textures / material folders) into one ordered file set. `scene.toml`
/// is always first.
pub fn assemble_bundle(
    scene: &Scene,
    assets: impl IntoIterator<Item = BundleFile>,
) -> Result<Vec<BundleFile>, toml::ser::Error> {
    let mut files = vec![BundleFile::new(
        SCENE_FILE,
        scene_to_toml(scene)?.into_bytes(),
    )];
    files.extend(assets);
    Ok(files)
}

/// Native: write a bundle file set to a directory (creating parent dirs).
#[cfg(feature = "fs-loader")]
pub fn write_bundle_dir(dir: &std::path::Path, files: &[BundleFile]) -> std::io::Result<()> {
    for f in files {
        let full = dir.join(&f.path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(full, &f.bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetEntry, AssetId, AssetSource, EditorNode, EnvironmentConfig, MaterialDef, MeshRef,
        MeshShadowConfig, NodeId, NodeKind, RuntimeMesh,
    };

    fn sample_scene() -> Scene {
        let mut scene = Scene {
            name: "demo".into(),
            ..Default::default()
        };
        // A glb-baked mesh asset + a primitive mesh asset + a material.
        let glb_mesh = AssetId::new();
        let prim_mesh = AssetId::new();
        let mat = AssetId::new();
        scene.assets.entries.insert(
            glb_mesh,
            AssetEntry::new(AssetSource::Mesh(RuntimeMesh::Glb)),
        );
        scene.assets.entries.insert(
            prim_mesh,
            AssetEntry::new(AssetSource::Mesh(RuntimeMesh::Primitive(
                crate::PrimitiveShape::Box {
                    dims: [1.0, 1.0, 1.0],
                },
            ))),
        );
        scene.assets.entries.insert(
            mat,
            AssetEntry::new(AssetSource::Material(MaterialDef::default())),
        );
        // A mesh node referencing the glb mesh + material.
        scene.nodes.push(EditorNode {
            id: NodeId::new(),
            name: "Hero".into(),
            transform: Default::default(),
            kind: NodeKind::Mesh {
                mesh: MeshRef(glb_mesh),
                material: None,
                shadow: MeshShadowConfig::default(),
            },
            locked: false,
            visible: true,
            prefab: false,
            children: vec![],
        });
        scene.environment = EnvironmentConfig::default();
        scene
    }

    #[test]
    fn scene_toml_round_trips() {
        let scene = sample_scene();
        let toml = scene_to_toml(&scene).expect("serialize scene.toml");
        let back = scene_from_toml(&toml).expect("parse scene.toml");
        assert_eq!(scene, back, "scene.toml round-trip");
    }

    #[test]
    fn assemble_puts_scene_first() {
        let scene = sample_scene();
        let glb = BundleFile::asset("abc.glb", vec![1, 2, 3]);
        let files = assemble_bundle(&scene, [glb.clone()]).unwrap();
        assert_eq!(files[0].path, SCENE_FILE);
        assert!(files[0].bytes.starts_with(b"name"));
        assert_eq!(files[1], glb);
    }
}
