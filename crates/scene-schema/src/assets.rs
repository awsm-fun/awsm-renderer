//! Project asset table.
//!
//! The scene refers to glb / gltf / KTX files only by `AssetId` (a UUID).
//! The table maps each id to its concrete source. For an editor project
//! on disk the source is `Filename` — the file lives at `assets/<filename>`
//! relative to the project directory. Build replaces those entries with
//! `Url` so the runtime knows where to fetch them from.
//!
//! Insert flow keys the table by *filename*: re-inserting `robot.glb` into
//! the same project resolves to the same `AssetId`, so the editor's
//! per-asset cache continues to dedup at the renderer level for free.

use std::collections::HashMap;
use uuid::Uuid;

use super::material::{MaterialDef, MeshDef, TextureDef};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
#[derive(Eq, Hash, Copy)]
pub struct AssetId(pub Uuid);

impl AssetId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for AssetId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetSource {
    /// Editor on-disk: file lives at `assets/<filename>` in the project dir.
    Filename(String),
    /// Build artifact: fetch from this URL at runtime.
    Url(String),
    /// Authored PBR (or unlit / toon) material parameters.
    Material(MaterialDef),
    /// Authored texture (raster file reference or procedural generator params).
    Texture(TextureDef),
    /// Procedural mesh placeholder (label only — actual mesh comes from the
    /// node that references it).
    Mesh(MeshDef),
}

impl AssetSource {
    pub fn filename(&self) -> Option<&str> {
        match self {
            Self::Filename(name) => Some(name.as_str()),
            Self::Url(_) => None,
            _ => None,
        }
    }

    pub fn is_file_backed(&self) -> bool {
        matches!(self, Self::Filename(_) | Self::Url(_))
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AssetEntry {
    pub source: AssetSource,
    /// Editable `MaterialDef` asset ids extracted from a glTF file on
    /// import, indexed by glTF material index. Empty for non-glTF
    /// entries (and for glTFs imported before this feature shipped —
    /// they continue to render via the gltf-baked materials).
    ///
    /// The `Model` instancer walks the per-primitive glTF material index
    /// in the matching `AssetTemplate` and, when an entry exists at
    /// that index, swaps the rendered mesh's material via
    /// `set_mesh_material`. This is the single source of truth — every
    /// Model node referencing the same gltf inherits the same overrides
    /// without per-node duplication.
    ///
    /// `#[serde(default)]` keeps existing project.json files
    /// round-tripping cleanly. We deliberately don't add
    /// `skip_serializing_if = "Vec::is_empty"` because bitcode (used
    /// for the per-game build artifact) doesn't support serde's skip
    /// hint — an empty Vec serializes as zero length anyway.
    #[serde(default)]
    pub gltf_material_asset_ids: Vec<AssetId>,
}

impl AssetEntry {
    /// Convenience for the common case: just a source, no glTF
    /// override map. Equivalent to `AssetEntry { source, ..Default }`
    /// but reads cleaner at call sites and stays valid as the struct
    /// grows.
    pub fn new(source: AssetSource) -> Self {
        Self {
            source,
            gltf_material_asset_ids: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
#[serde(transparent)]
pub struct AssetTable {
    pub entries: HashMap<AssetId, AssetEntry>,
}

impl AssetTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: AssetId) -> Option<&AssetEntry> {
        self.entries.get(&id)
    }

    pub fn filename(&self, id: AssetId) -> Option<&str> {
        self.entries.get(&id).and_then(|e| e.source.filename())
    }

    /// Look up an `AssetId` by exact filename match. Used by the editor's
    /// Insert flows to dedup re-imports of the same file in a session.
    pub fn find_by_filename(&self, filename: &str) -> Option<AssetId> {
        self.entries
            .iter()
            .find_map(|(id, entry)| match &entry.source {
                AssetSource::Filename(name) if name == filename => Some(*id),
                _ => None,
            })
    }

    /// Insert a filename-backed entry, reusing an existing `AssetId` if
    /// the filename is already in the table.
    pub fn insert_filename(&mut self, filename: String) -> AssetId {
        if let Some(id) = self.find_by_filename(&filename) {
            return id;
        }
        let id = AssetId::new();
        self.entries
            .insert(id, AssetEntry::new(AssetSource::Filename(filename)));
        id
    }

    pub fn remove(&mut self, id: AssetId) {
        self.entries.remove(&id);
    }
}
