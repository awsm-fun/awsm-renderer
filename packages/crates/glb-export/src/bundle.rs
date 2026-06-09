//! Player-runtime **bundle**: lay out a baked scene plus its sidecars
//! (custom-material WGSL/TOML, referenced textures, environment) into a directory
//! tree the game-player loads via a single `bundle.json` index.
//!
//! The **assembly + layout is pure data** ([`assemble_bundle`]) so it's natively
//! testable; the editor gathers the pieces (the whole-scene `scene.glb` from the
//! GPU-backed exporter, the material side-files, texture readback) and then either
//! writes the files to disk ([`PlayerBundle::write_to_dir`], native) or — in the
//! browser — streams each [`BundleFile`] out through its FS-Access handle / an MCP
//! manifest. The **player-side loader lives in the separate game-player repo** and
//! is out of scope here.

use std::path::Path;

/// One file in a player bundle: a bundle-relative path + its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFile {
    /// Path relative to the bundle root, always forward-slashed (e.g.
    /// `textures/checker.png`). Never absolute, never contains `..`.
    pub path: String,
    pub bytes: Vec<u8>,
}

/// The pieces the editor hands [`assemble_bundle`].
#[derive(Clone, Debug, Default)]
pub struct BundleInputs {
    /// The whole-scene baked glTF (from [`crate::write_glb`]).
    pub scene_glb: Vec<u8>,
    /// Custom-material side-files: `(bundle-relative path, text content)` — the
    /// `.wgsl` + `.toml` an `AWSM_materials_none` primitive resolves against.
    /// Paths are taken verbatim (the editor roots them under `materials/<id>/…`).
    pub materials: Vec<(String, String)>,
    /// Referenced textures: `(filename, encoded bytes)`, placed under `textures/`.
    pub textures: Vec<(String, Vec<u8>)>,
    /// Serialized environment descriptor (skybox / IBL), if any → `env.json`.
    pub env_json: Option<String>,
}

/// A laid-out player bundle: every file with its bundle-relative path, plus a
/// `bundle.json` index. Write it with [`Self::write_to_dir`] (native) or stream
/// the files however the host prefers.
#[derive(Clone, Debug, Default)]
pub struct PlayerBundle {
    pub files: Vec<BundleFile>,
}

/// Lay out the bundle: `scene.glb`, the material side-files at their given
/// relative paths, the textures under `textures/`, `env.json` (when present), and
/// a `bundle.json` manifest indexing them all so the player loads from one entry
/// point.
pub fn assemble_bundle(name: &str, inputs: BundleInputs) -> PlayerBundle {
    let mut files = Vec::new();

    files.push(BundleFile {
        path: "scene.glb".to_string(),
        bytes: inputs.scene_glb,
    });

    let material_paths: Vec<String> = inputs.materials.iter().map(|(p, _)| p.clone()).collect();
    for (path, content) in inputs.materials {
        files.push(BundleFile {
            path,
            bytes: content.into_bytes(),
        });
    }

    let mut texture_paths = Vec::new();
    for (filename, bytes) in inputs.textures {
        let path = format!("textures/{filename}");
        texture_paths.push(path.clone());
        files.push(BundleFile { path, bytes });
    }

    let env_path = inputs.env_json.as_ref().map(|_| "env.json".to_string());
    if let Some(env) = inputs.env_json {
        files.push(BundleFile {
            path: "env.json".to_string(),
            bytes: env.into_bytes(),
        });
    }

    // The manifest goes last so the player has a single index over the rest.
    let manifest = serde_json::json!({
        "name": name,
        "scene": "scene.glb",
        "materials": material_paths,
        "textures": texture_paths,
        "env": env_path,
    });
    files.push(BundleFile {
        path: "bundle.json".to_string(),
        bytes: serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    });

    PlayerBundle { files }
}

impl PlayerBundle {
    /// Write every file under `dir`, creating parent directories as needed.
    /// Native (`std::fs`); the editor writes through its browser FS handle
    /// instead. Rejects any path that isn't bundle-relative (absolute or with a
    /// `..` component) so a crafted material name can't escape `dir`.
    pub fn write_to_dir(&self, dir: &Path) -> std::io::Result<()> {
        for f in &self.files {
            let rel = Path::new(&f.path);
            let safe = rel.is_relative()
                && !rel
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir));
            if !safe {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("unsafe bundle path: {}", f.path),
                ));
            }
            let full = dir.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, &f.bytes)?;
        }
        Ok(())
    }
}
