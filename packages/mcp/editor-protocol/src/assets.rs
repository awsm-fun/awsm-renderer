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

use crate::mesh_def::MeshDef;
use awsm_renderer_scene::{mesh_asset_filename, AssetId, MaterialDef, TextureDef};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
// `Material(MaterialDef)` is intentionally the large variant (it carries the full
// authored PBR surface incl. KHR extensions). Boxing it would ripple through every
// player/editor match site for marginal benefit; assets are not stored in hot,
// densely-packed arrays.
#[allow(clippy::large_enum_variant)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    /// Raw little-endian `u32` buffer data bound into a custom-material buffer
    /// slot (via `set_material_buffer`). Content-addressed like a raster texture:
    /// the bytes live at `assets/<content_hash>.bin` (the editor caches them until
    /// Save) and dedup across instances. Editor-only — the bake resolves a buffer
    /// override by its asset-id filename, so this entry isn't carried to the
    /// runtime asset table.
    Buffer(BufferDef),
    /// Procedural mesh placeholder (label only — actual mesh comes from the
    /// node that references it).
    Mesh(MeshDef),
}

/// Authoring metadata for an [`AssetSource::Buffer`] entry. The bytes themselves
/// are content-addressed on disk (`assets/<content_hash>.bin`); this carries only
/// what the UI / `get_node_details` want to show without loading the file.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BufferDef {
    /// Number of little-endian `u32` words in the buffer.
    #[serde(default)]
    pub word_len: u32,
}

impl AssetSource {
    /// User-facing display name. Filename + Raster texture entries
    /// return the original upload name; everything else (Url,
    /// Material, Procedural texture, Mesh) returns `None`.
    pub fn display_name(&self) -> Option<&str> {
        match self {
            Self::Filename(name) => Some(name.as_str()),
            Self::Texture(TextureDef::Raster { display_name, .. }) => Some(display_name.as_str()),
            _ => None,
        }
    }

    pub fn is_file_backed(&self) -> bool {
        matches!(self, Self::Filename(_) | Self::Url(_))
    }
}

/// Per-texture choice of how the bundle bake encodes this texture's image. An
/// AUTHORING preference, persisted per texture in the project (`project.toml`)
/// and read by the bundle bake — distinct from the runtime
/// [`TextureEncoding`](awsm_renderer_scene::TextureEncoding), which records the
/// RESULT the bake produced and travels in the bundle.
///
/// `None` on the asset entry — and any project saved before this field existed —
/// means the default [`WebpLossless`](Self::WebpLossless): every raster texture
/// ships as lossless WebP (pixel-identical to the source, smaller than PNG,
/// decoded by the player exactly like PNG) unless the author opts a specific
/// texture down to lossy or out to its verbatim source bytes.
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TextureExport {
    /// Ship the source bytes verbatim under their real extension — no re-encode.
    /// Use for a texture already in an optimal format, or to preserve the exact
    /// source bytes.
    Source,
    /// Re-encode to lossless WebP (the default): pixel-identical to the source,
    /// typically smaller than PNG, browser-decodable.
    #[default]
    WebpLossless,
    /// Re-encode to lossy WebP at `quality` (0.0..=1.0). Smaller still, at a
    /// visible-quality cost the author accepts for this texture. Higher = larger,
    /// closer to lossless.
    WebpLossy { quality: f32 },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    /// For a texture asset (`AssetSource::Texture`), the author's chosen
    /// bundle-bake encoding (see [`TextureExport`]). `None` (untouched textures,
    /// and projects saved before this field existed) means the default
    /// [`TextureExport::WebpLossless`], so every raster texture bakes as lossless
    /// WebP unless overridden here. Ignored for non-texture entries.
    /// `#[serde(default)]` keeps pre-field `project.json` files round-tripping.
    /// We deliberately don't add `skip_serializing_if = "Option::is_none"`
    /// because bitcode (used for the per-game build artifact) doesn't support
    /// serde's skip hint — a `None` Option serializes as a zero discriminant
    /// anyway. See `gltf_material_asset_ids` above.
    #[serde(default)]
    pub texture_export: Option<TextureExport>,
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
            texture_export: None,
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
            texture_export: None,
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
    if let AssetSource::Mesh(_) = &entry.source {
        return Some(mesh_asset_filename(id));
    }
    if entry.content_hash.is_empty() {
        return None;
    }
    // Buffer data is content-addressed as `<content_hash>.bin` (no display name /
    // extension to derive from, unlike a file-backed texture).
    if let AssetSource::Buffer(_) = &entry.source {
        return Some(format!("{}.bin", entry.content_hash));
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
