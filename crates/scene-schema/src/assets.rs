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
    /// Editor on-disk file (glb / gltf / ktx). The inner String is the
    /// user's original filename, kept for UI labels and to derive the
    /// file extension — the on-disk path is computed from the
    /// containing `AssetEntry::content_hash` (see
    /// [`asset_disk_path`]), not from this string.
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
    /// User-facing display name. Filename + Raster texture entries
    /// return the original upload name; everything else (Url,
    /// Material, Procedural texture, Mesh) returns `None`.
    pub fn display_name(&self) -> Option<&str> {
        match self {
            Self::Filename(name) => Some(name.as_str()),
            Self::Texture(crate::material::TextureDef::Raster { display_name }) => {
                Some(display_name.as_str())
            }
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
    /// SHA-256 of the on-disk file's content, as lowercase hex. Drives
    /// the disk path (see [`asset_disk_path`]) and the upload-time
    /// dedup check — two uploads of identical bytes reuse the same
    /// `AssetId` instead of clobbering each other on save.
    ///
    /// Empty for non-file-backed sources (`Material`, `Procedural`
    /// textures, captured `Mesh` blobs — those use other addressing
    /// schemes). `#[serde(default)]` so the field reads cleanly from
    /// migrated or hand-edited project.json files.
    #[serde(default)]
    pub content_hash: String,
}

impl AssetEntry {
    /// Convenience for the common case: just a source, no glTF
    /// override map, no content hash. Used for non-file-backed
    /// sources (`Material`, `Procedural` textures, captured `Mesh`s)
    /// where `content_hash` is unused.
    pub fn new(source: AssetSource) -> Self {
        Self {
            source,
            gltf_material_asset_ids: Vec::new(),
            content_hash: String::new(),
        }
    }

    /// Construct a file-backed entry with its content hash already
    /// computed. Use this for `AssetSource::Filename` and
    /// `AssetSource::Texture(TextureDef::Raster { .. })` — the hash
    /// is what addresses the on-disk file and powers dedup.
    pub fn new_with_hash(source: AssetSource, content_hash: String) -> Self {
        Self {
            source,
            gltf_material_asset_ids: Vec::new(),
            content_hash,
        }
    }
}

/// Disk path (relative to the project's `assets/` directory) for a
/// file-backed asset entry. Returns `None` for entries that don't
/// live on disk (`Material`, `Procedural` textures, `Url`) or are
/// missing the content hash. Uses the entry's `display_name` to pull
/// the file extension so a hashed file keeps its original extension —
/// browsers + image decoders treat `<hash>.png` the same as
/// `smoke.png` but the on-disk layout is collision-free.
///
/// Captured procedural meshes (`AssetSource::Mesh`) keep their
/// historical `<asset-id>.mesh.bin` path — see
/// [`mesh_asset_filename`] — they're addressed by `AssetId` because
/// the bytes are deterministic from the `MeshDef`, not user-uploaded.
pub fn asset_disk_path(id: AssetId, entry: &AssetEntry) -> Option<String> {
    use crate::material::{mesh_asset_filename, TextureDef};
    if let AssetSource::Mesh(_) = &entry.source {
        return Some(format!("assets/{}", mesh_asset_filename(id)));
    }
    if entry.content_hash.is_empty() {
        return None;
    }
    let display = match &entry.source {
        AssetSource::Filename(name) => name.as_str(),
        AssetSource::Texture(TextureDef::Raster { display_name }) => display_name.as_str(),
        _ => return None,
    };
    let ext = display.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if ext.is_empty() {
        Some(format!("assets/{}", entry.content_hash))
    } else {
        Some(format!("assets/{}.{}", entry.content_hash, ext))
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

    /// User-facing display name for an asset. For file-backed
    /// sources this is the *original* upload name (not the hashed
    /// on-disk filename) — useful for the UI label + for deriving the
    /// file extension when computing the disk path.
    pub fn display_name(&self, id: AssetId) -> Option<&str> {
        self.entries.get(&id).and_then(|e| e.source.display_name())
    }

    /// Look up an `AssetId` by exact content-hash match. Used by every
    /// upload path (image picker, glTF importer, embedded-image
    /// extractor) to dedup re-uploads of identical bytes within the
    /// same project.
    pub fn find_by_content_hash(&self, hash: &str) -> Option<AssetId> {
        if hash.is_empty() {
            return None;
        }
        self.entries
            .iter()
            .find_map(|(id, entry)| (entry.content_hash == hash).then_some(*id))
    }

    /// Insert a file-backed `Filename` entry with its content hash,
    /// reusing an existing `AssetId` if the same content is already in
    /// the table. Used by glTF / glb imports (image / KTX imports
    /// build their own `Raster` entries through the same dedup helper
    /// pattern at the call site).
    pub fn insert_file_with_hash(&mut self, display_name: String, content_hash: String) -> AssetId {
        if let Some(id) = self.find_by_content_hash(&content_hash) {
            return id;
        }
        let id = AssetId::new();
        self.entries.insert(
            id,
            AssetEntry::new_with_hash(AssetSource::Filename(display_name), content_hash),
        );
        id
    }

    pub fn remove(&mut self, id: AssetId) {
        self.entries.remove(&id);
    }
}
