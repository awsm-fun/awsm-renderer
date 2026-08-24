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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::assets::AssetId;
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

/// Bundle/project-relative path of an environment KTX2 cubemap asset —
/// `assets/<id>.ktx2`. THE single naming convention shared by every reader and
/// writer: the editor's Save (`ktx_files`) and project reload (`restore_ktx`),
/// the player-bundle bake, and the player's `apply_environment` all resolve an
/// env KTX through this function, so the name cannot drift between them (a
/// drift would silently play the built-in default environment).
pub fn env_ktx_path(id: AssetId) -> String {
    format!("{ASSETS_DIR}/{}.ktx2", id.0)
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

/// Bundle-relative path of the `index`-th asset-pack blob.
pub fn pack_path(index: usize) -> String {
    format!("{ASSETS_DIR}/pack-{index}.bin")
}

/// Aim for blobs of roughly this many bytes; a handful of pack fetches keeps
/// the load bandwidth-bound (each blob big enough to saturate its stream)
/// while still giving the progress UI real ticks and letting the streams
/// share the pipe on high-latency links.
const PACK_TARGET_BYTES: u64 = 8 * 1024 * 1024;
/// Never split into more blobs than this — beyond a handful the per-request
/// tax packing exists to remove starts creeping back in.
const PACK_MAX_BLOBS: usize = 8;

/// The bundle's asset-pack index, carried inside `scene.toml` (see
/// [`Scene::pack`](crate::Scene::pack)): where every packed `assets/` file
/// lives inside the concatenated `assets/pack-<n>.bin` blobs.
///
/// A cold world load of a loose-file bundle is hundreds of latency-bound
/// requests (one per mesh glb); a packed bundle is the SAME bytes in a few
/// blob fetches. The index — not a sidecar manifest — rides `scene.toml`
/// because the loader has already parsed the scene when it decides what to
/// fetch: no probe request, and the index is atomic with the scene that
/// references the packed files.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScenePack {
    /// Byte length of each pack blob; `packs[i]` describes [`pack_path`]`(i)`.
    /// Lengths let a consumer sanity-check a fetched blob and weight progress.
    pub packs: Vec<u64>,
    /// Bundle-relative path → `(pack index, byte offset, byte length)` inside
    /// that blob. A `BTreeMap` so `scene.toml` serialization is deterministic
    /// (byte-stable re-exports).
    pub files: BTreeMap<String, (u32, u64, u64)>,
}

impl ScenePack {
    /// Slice one fetched pack blob back into its member files. Yields
    /// `(path, Some(bytes))` for members whose recorded range fits the blob
    /// and `(path, None)` for corrupt/out-of-range entries (a caller caches
    /// the failure so the consumption site sees the same missing-asset error
    /// a lost file would produce).
    pub fn slice_blob<'a>(
        &'a self,
        pack_index: usize,
        blob: &'a [u8],
    ) -> impl Iterator<Item = (&'a str, Option<&'a [u8]>)> + 'a {
        self.files
            .iter()
            .filter(move |(_, &(p, _, _))| p as usize == pack_index)
            .map(move |(path, &(_, off, len))| {
                let bytes = usize::try_from(off)
                    .ok()
                    .zip(usize::try_from(len).ok())
                    .and_then(|(off, len)| blob.get(off..off.checked_add(len)?));
                (path.as_str(), bytes)
            })
    }
}

/// Pack `assets` into a few concatenated blobs and assemble the bundle:
/// `scene.toml` (now carrying the [`ScenePack`] index) first, then the
/// `assets/pack-<n>.bin` blobs, then whatever stayed loose.
///
/// What packs: every `assets/` file EXCEPT `.clusters.bin` DAGs and the
/// environment STREAMING-ladder cubemaps — both stream on demand by design
/// (clusters via the DAG walk, env ladders via the post-load environment
/// stream), so packing either would front-load bytes the load may never
/// need (a 2048-face skybox level is the entire point of NOT blocking on
/// it). Blob assignment is deterministic (size-descending greedy onto the
/// lightest blob, ties broken by path), so re-exporting an unchanged scene
/// is byte-stable.
pub fn assemble_bundle_packed(
    scene: &mut Scene,
    assets: impl IntoIterator<Item = BundleFile>,
) -> Result<Vec<BundleFile>, toml::ser::Error> {
    let stream_paths: std::collections::HashSet<String> = scene
        .environment
        .stream
        .asset_ids()
        .map(|id| env_ktx_path(*id))
        .collect();
    let (packable, loose): (Vec<BundleFile>, Vec<BundleFile>) = assets.into_iter().partition(|f| {
        f.path.starts_with(ASSETS_DIR)
            && !f.path.ends_with(".clusters.bin")
            && !stream_paths.contains(&f.path)
    });

    if packable.is_empty() {
        scene.pack = None;
        return assemble_bundle(scene, loose);
    }

    let total: u64 = packable.iter().map(|f| f.bytes.len() as u64).sum();
    let blob_count = usize::try_from(total.div_ceil(PACK_TARGET_BYTES))
        .unwrap_or(PACK_MAX_BLOBS)
        .clamp(1, PACK_MAX_BLOBS);

    // Size-descending greedy onto the lightest blob balances blob sizes so
    // the concurrent blob fetches finish together (progress ticks stay even).
    let mut order: Vec<&BundleFile> = packable.iter().collect();
    order.sort_by(|a, b| {
        b.bytes
            .len()
            .cmp(&a.bytes.len())
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut blobs: Vec<Vec<u8>> = vec![Vec::new(); blob_count];
    let mut files: BTreeMap<String, (u32, u64, u64)> = BTreeMap::new();
    for file in order {
        let lightest = (0..blob_count)
            .min_by_key(|&i| blobs[i].len())
            .expect("blob_count >= 1");
        let blob = &mut blobs[lightest];
        files.insert(
            file.path.clone(),
            (lightest as u32, blob.len() as u64, file.bytes.len() as u64),
        );
        blob.extend_from_slice(&file.bytes);
    }

    scene.pack = Some(ScenePack {
        packs: blobs.iter().map(|b| b.len() as u64).collect(),
        files,
    });

    let packed_files = blobs
        .into_iter()
        .enumerate()
        .map(|(i, bytes)| BundleFile::new(pack_path(i), bytes));
    assemble_bundle(scene, packed_files.chain(loose))
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
        AssetEntry, AssetId, AssetSource, EditorNode, EnvironmentConfig, MaterialDef,
        MeshLodConfig, MeshRef, MeshShadowConfig, NodeId, NodeKind, RuntimeMesh,
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
            physics: None,
            mujoco: None,
            id: NodeId::new(),
            name: "Hero".into(),
            transform: Default::default(),
            kind: NodeKind::Mesh {
                mesh: MeshRef(glb_mesh),
                material_variants: Vec::new(),
                selected_variant: None,
                shadow: MeshShadowConfig::default(),
                lod: MeshLodConfig::default(),
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

    /// A KTX environment must survive the bundle round-trip END-TO-END at the
    /// file level: the `scene.toml` carries the exact skybox/IBL asset ids, and
    /// for each id the bundle holds a file at [`env_ktx_path`] — the SAME path
    /// the player's `apply_environment` fetches. This is the headless half of
    /// the player-path guarantee (the GPU render itself is browser-verified);
    /// it catches both an env config dropped from `scene.toml` and a naming
    /// drift between the bake and the loader.
    #[test]
    fn ktx_environment_round_trips_through_bundle() {
        let mut scene = sample_scene();
        let skybox = AssetId::new();
        let prefiltered = AssetId::new();
        let irradiance = AssetId::new();
        scene.environment = EnvironmentConfig {
            skybox: crate::EnvSlot::Ktx { asset_id: skybox },
            specular: crate::EnvSlot::Ktx {
                asset_id: prefiltered,
            },
            irradiance: crate::EnvSlot::Ktx {
                asset_id: irradiance,
            },
            probe: Default::default(),
            rotation: Default::default(),
            // A skybox streaming ladder: its levels must ship in the bundle
            // exactly like the base slots' files.
            stream: crate::EnvStream {
                skybox: vec![AssetId::new()],
                ..Default::default()
            },
        };
        // The bake emits one file per env KTX id, at the shared convention path.
        let env_files: Vec<BundleFile> = scene
            .environment
            .ktx_asset_ids()
            .into_iter()
            .map(|id| BundleFile::new(env_ktx_path(id), vec![0xAB]))
            .collect();
        assert_eq!(
            env_files.len(),
            4,
            "skybox + prefiltered + irradiance + one streamed skybox level"
        );
        let files = assemble_bundle(&scene, env_files).unwrap();

        // Player side: parse scene.toml back and resolve every env KTX id to a
        // bundle file via the same `env_ktx_path` the loader uses.
        let toml = std::str::from_utf8(&files[0].bytes).unwrap();
        let loaded = scene_from_toml(toml).expect("parse scene.toml");
        assert_eq!(loaded.environment, scene.environment, "env config intact");
        for id in loaded.environment.ktx_asset_ids() {
            let path = env_ktx_path(id);
            assert!(
                files.iter().any(|f| f.path == path),
                "bundle must contain {path}"
            );
        }

        // PACKED assembly: the streamed level must stay a LOOSE file (it is
        // fetched on demand after load), while the base env cubemaps pack.
        let env_files: Vec<BundleFile> = scene
            .environment
            .ktx_asset_ids()
            .into_iter()
            .map(|id| BundleFile::new(env_ktx_path(id), vec![0xAB]))
            .collect();
        let stream_path = env_ktx_path(scene.environment.stream.skybox[0]);
        let packed = assemble_bundle_packed(&mut scene, env_files).unwrap();
        assert!(
            packed.iter().any(|f| f.path == stream_path),
            "streamed env level must ship as a loose file, not inside a pack blob"
        );
    }

    /// Packed-bundle round trip END-TO-END at the file level: assemble with
    /// packing, parse the emitted `scene.toml` back (the player's first step),
    /// and reconstruct every packed member from the blobs via the SAME
    /// [`ScenePack::slice_blob`] the loader uses. Every asset byte must
    /// survive, the cluster DAG must stay loose (it streams on demand), and
    /// the loose-file `assemble_bundle` path must be untouched by the field.
    #[test]
    fn packed_bundle_round_trips_through_blobs() {
        let mut scene = sample_scene();
        let assets = vec![
            BundleFile::asset("aaa.glb", vec![1u8; 10]),
            BundleFile::asset("bbb.glb", (0..=255).collect()),
            BundleFile::asset("ccc.ktx2", vec![7u8; 4096]),
            BundleFile::new("assets/materials/glow/material.wgsl", b"// wgsl".to_vec()),
            BundleFile::asset("ddd.clusters.bin", vec![9u8; 64]),
        ];
        let originals: Vec<BundleFile> = assets.clone();
        let files = assemble_bundle_packed(&mut scene, assets).unwrap();

        // scene.toml first, carrying the index.
        assert_eq!(files[0].path, SCENE_FILE);
        let toml = std::str::from_utf8(&files[0].bytes).unwrap();
        let loaded = scene_from_toml(toml).expect("parse packed scene.toml");
        let pack = loaded.pack.as_ref().expect("pack index present");

        // The cluster DAG ships loose, is absent from the index, and nothing
        // else survived as a loose asset file.
        assert!(files.iter().any(|f| f.path == "assets/ddd.clusters.bin"));
        assert!(!pack.files.keys().any(|p| p.ends_with(".clusters.bin")));
        for f in &files[1..] {
            assert!(
                f.path.starts_with("assets/pack-") || f.path.ends_with(".clusters.bin"),
                "unexpected loose file {}",
                f.path
            );
        }

        // Blob lengths match the index, and every packed member reconstructs
        // byte-identically through slice_blob.
        let blobs: Vec<&BundleFile> = (0..pack.packs.len())
            .map(|i| {
                files
                    .iter()
                    .find(|f| f.path == pack_path(i))
                    .expect("pack blob emitted")
            })
            .collect();
        for (i, blob) in blobs.iter().enumerate() {
            assert_eq!(blob.bytes.len() as u64, pack.packs[i]);
        }
        let mut reconstructed: BTreeMap<&str, &[u8]> = BTreeMap::new();
        for (i, blob) in blobs.iter().enumerate() {
            for (path, bytes) in pack.slice_blob(i, &blob.bytes) {
                reconstructed.insert(path, bytes.expect("in-range slice"));
            }
        }
        for orig in originals
            .iter()
            .filter(|f| !f.path.ends_with(".clusters.bin"))
        {
            assert_eq!(
                reconstructed.get(orig.path.as_str()).copied(),
                Some(orig.bytes.as_slice()),
                "{} must reconstruct byte-identically",
                orig.path
            );
        }

        // Determinism: a second identical assemble is byte-identical.
        let mut scene2 = sample_scene();
        scene2.name = scene.name.clone();
        let again = assemble_bundle_packed(&mut scene2, originals.clone()).unwrap();
        // scene ids differ between sample_scene() calls, so compare the pack
        // blobs + index only (the deterministic part under test).
        assert_eq!(scene2.pack, scene.pack);
        for (i, _) in blobs.iter().enumerate() {
            let a = files.iter().find(|f| f.path == pack_path(i)).unwrap();
            let b = again.iter().find(|f| f.path == pack_path(i)).unwrap();
            assert_eq!(a.bytes, b.bytes, "pack blob {i} must be byte-stable");
        }

        // A corrupt index entry slices to None, never panics.
        let mut corrupt = loaded.pack.clone().unwrap();
        corrupt
            .files
            .insert("assets/oob.glb".into(), (0, u64::MAX - 1, 16));
        let (path, bytes) = corrupt
            .slice_blob(0, &blobs[0].bytes)
            .find(|(p, _)| *p == "assets/oob.glb")
            .unwrap();
        assert_eq!((path, bytes), ("assets/oob.glb", None));
    }

    /// No packable assets → no pack index, plain loose assemble.
    #[test]
    fn empty_pack_stays_loose() {
        let mut scene = sample_scene();
        let cluster = BundleFile::asset("x.clusters.bin", vec![1, 2, 3]);
        let files = assemble_bundle_packed(&mut scene, [cluster.clone()]).unwrap();
        assert!(scene.pack.is_none());
        assert_eq!(files.len(), 2);
        assert_eq!(files[1], cluster);
        let loaded = scene_from_toml(std::str::from_utf8(&files[0].bytes).unwrap()).unwrap();
        assert!(loaded.pack.is_none());
    }

    /// The procedural sky-gradient environment is pure config — it must
    /// round-trip through `scene.toml` with no side files at all.
    #[test]
    fn gradient_environment_round_trips_through_scene_toml() {
        let mut scene = sample_scene();
        let grad = crate::EnvSlot::SkyGradient {
            zenith: [0.9, 0.3, 0.1],
            nadir: [0.05, 0.02, 0.1],
        };
        scene.environment = EnvironmentConfig {
            skybox: grad,
            specular: grad,
            irradiance: grad,
            probe: Default::default(),
            stream: Default::default(),
            // Per-slot rotations must survive the SAME scene.toml the player
            // bundle carries — the bundle half of "authored in the editor,
            // seen in the player". Deliberately DIFFERENT per slot, so a
            // bundle that collapsed them to one value fails here.
            rotation: crate::EnvRotation {
                skybox: [0.0, 137.5, 0.0],
                specular: [0.0, -40.0, 0.0],
                irradiance: [12.0, 0.0, 0.0],
            },
        };
        let toml = scene_to_toml(&scene).unwrap();
        let loaded = scene_from_toml(&toml).unwrap();
        assert_eq!(loaded.environment, scene.environment);
        assert_eq!(
            loaded.environment.rotation, scene.environment.rotation,
            "per-slot env rotations must survive the bundled scene.toml"
        );
        assert!(
            loaded.environment.ktx_asset_ids().is_empty(),
            "gradient env references no KTX assets"
        );
    }
}
