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

use super::material::{MaterialDef, TextureDef};
use super::mesh::RuntimeMesh;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
#[derive(Eq, Hash, Copy)]
pub struct AssetId(pub Uuid);

// A `AssetId` is a UUID string on the wire — describe it as such for JSON Schema
// (used by the MCP server's typed tool params) rather than recursing into Uuid.
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for AssetId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AssetId".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "format": "uuid" })
    }
}

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
// `Material(MaterialDef)` is intentionally the large variant (it carries the full
// authored PBR surface incl. KHR extensions). Boxing it would ripple through every
// player/editor match site for marginal benefit; assets are not stored in hot,
// densely-packed arrays.
#[allow(clippy::large_enum_variant)]
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
    /// A runtime mesh — a procedural primitive (regenerated at load) or a baked
    /// geometry blob (`assets/<id>.mesh.bin`). The authoring `MeshDef` (modifier
    /// stack + per-vertex overrides) lowers to this at bake time and lives in
    /// `awsm-renderer-editor-protocol`.
    Mesh(RuntimeMesh),
}

impl AssetSource {
    /// User-facing display name. Filename + Raster texture entries
    /// return the original upload name; everything else (Url,
    /// Material, Procedural texture, Mesh) returns `None`.
    pub fn display_name(&self) -> Option<&str> {
        match self {
            Self::Filename(name) => Some(name.as_str()),
            Self::Texture(crate::material::TextureDef::Raster { display_name, .. }) => {
                Some(display_name.as_str())
            }
            _ => None,
        }
    }

    pub fn is_file_backed(&self) -> bool {
        matches!(self, Self::Filename(_) | Self::Url(_))
    }
}

/// The on-disk encoding of a bundle texture image — recorded per texture asset
/// so the player derives the file extension and decode path from DATA, never
/// from a hardcoded `.png` or content-sniffing.
///
/// This is what lets new formats land without breaking older bundles: a bundle
/// that predates the field (or a procedurally-generated PNG) deserializes as the
/// default [`Png`](Self::Png), so old bundles keep loading `<id>.png`; a bundle
/// that ships WebP records [`Webp`](Self::Webp) and the loader fetches
/// `<id>.webp`. The split between `browser_decodable` rasters and GPU-compressed
/// containers is intrinsic to the format, so it lives here, not in the loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TextureEncoding {
    #[default]
    Png,
    Jpeg,
    Webp,
    Ktx2,
}

impl TextureEncoding {
    /// Map a bundle file extension (no dot, any case) to an encoding, or `None`
    /// for one we don't handle.
    pub fn from_ext(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            "webp" => Self::Webp,
            "ktx2" => Self::Ktx2,
            _ => return None,
        })
    }

    /// The bundle file extension (no dot) for this encoding.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Ktx2 => "ktx2",
        }
    }

    /// True iff a web browser can decode this directly (`createImageBitmap`), so
    /// the player's zero-copy URL path is valid. GPU-compressed containers
    /// (KTX2/basis) return `false`: only a transcoder understands them, so their
    /// bytes must transit wasm even when a URL exists.
    pub fn browser_decodable(self) -> bool {
        match self {
            Self::Png | Self::Jpeg | Self::Webp => true,
            Self::Ktx2 => false,
        }
    }

    /// MIME type (for decoding a byte blob via `createImageBitmap`).
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Ktx2 => "image/ktx2",
        }
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
    /// Editable `Texture` asset ids extracted from a glTF file on
    /// import, indexed by glTF *image* index (not texture index —
    /// multiple glTF textures can share the same image with
    /// different samplers, but the editor stores one Texture asset
    /// per image so the assets library stays tidy). Empty for
    /// non-glTF entries (and for glTFs imported before this
    /// feature shipped).
    ///
    /// Used at populate-glb time to seed the editor's
    /// `texture_cache` with the `TextureKey`s the renderer-gltf
    /// side already uploaded — without this, every editor
    /// material override would re-decode + re-upload the same
    /// image, doubling GPU storage and decode wall-clock per glTF
    /// texture. See `crates/frontend/scene-editor/src/renderer_bridge/asset_cache.rs::seed_texture_cache_from_populate`.
    ///
    /// Vec position is the glTF image index; gaps are filled with
    /// `AssetId::default()` if the document skips one (rare —
    /// glTF documents almost always pack image indices densely).
    /// `#[serde(default)]` keeps pre-feature project.json files
    /// round-tripping cleanly.
    #[serde(default)]
    pub gltf_image_asset_ids: Vec<AssetId>,
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
    /// For a texture asset (`AssetSource::Texture`), the on-disk encoding of the
    /// image the bundle ships at `assets/<id>.<ext>` — the player derives the
    /// extension and decode path from this, not a hardcoded `.png`. `None` for
    /// non-texture entries and for bundles baked before the field existed; the
    /// loader treats `None` as [`TextureEncoding::Png`] (the legacy default), so
    /// old bundles keep loading unchanged. Set by the bundle bake from the
    /// texture's source MIME.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture_encoding: Option<TextureEncoding>,
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
            gltf_image_asset_ids: Vec::new(),
            content_hash: String::new(),
            texture_encoding: None,
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
            gltf_image_asset_ids: Vec::new(),
            content_hash,
            texture_encoding: None,
        }
    }
}

/// Leaf filename (no directory prefix) for a file-backed asset
/// entry. Format is `<content_hash>.<ext>` where the extension is
/// derived from the entry's `display_name`. Captured procedural
/// meshes return `<asset-id>.mesh.bin` instead — they're addressed
/// by `AssetId` because the bytes are deterministic from the
/// `MeshDef`, not user-uploaded.
///
/// Returns `None` for entries that don't live on disk (`Material`,
/// `Procedural` textures, `Url`) or are missing `content_hash`.
///
/// This is the unit both the editor (for project-relative paths
/// under `assets/`) and the player (for CDN URLs under
/// `<base>/games/<gid>/world/assets/`) build on.
pub fn asset_filename(id: AssetId, entry: &AssetEntry) -> Option<String> {
    use crate::material::{mesh_asset_filename, TextureDef};
    if let AssetSource::Mesh(_) = &entry.source {
        return Some(mesh_asset_filename(id));
    }
    if entry.content_hash.is_empty() {
        return None;
    }
    let display = match &entry.source {
        AssetSource::Filename(name) => name.as_str(),
        AssetSource::Texture(TextureDef::Raster { display_name, .. }) => display_name.as_str(),
        _ => return None,
    };
    let ext = display.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    Some(if ext.is_empty() {
        entry.content_hash.clone()
    } else {
        format!("{}.{}", entry.content_hash, ext)
    })
}

/// Project-relative disk path (`assets/<leaf>`) for a file-backed
/// asset entry. Thin wrapper over [`asset_filename`] for editor save
/// / load callers; the player composes its own CDN URL out of the
/// leaf directly.
pub fn asset_disk_path(id: AssetId, entry: &AssetEntry) -> Option<String> {
    asset_filename(id, entry).map(|leaf| format!("assets/{leaf}"))
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
