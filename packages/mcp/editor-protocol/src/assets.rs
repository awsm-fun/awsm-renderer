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
/// means the default [`Ktx2`](Self::Ktx2) with the [`Auto`](Ktx2Profile::Auto)
/// profile: every raster texture bakes to a Basis-supercompressed KTX2 that the
/// player transcodes to a native GPU block format (BC/ASTC/ETC2 — 4–8× less
/// VRAM than RGBA8) unless the author opts a specific texture to WebP or its
/// verbatim source bytes. ⚠ This default CHANGED from lossless WebP with the
/// compression feature (docs/plans/compression.md) — the accepted migration is
/// that existing projects re-bake their textures as KTX2 on the next export.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TextureExport {
    /// Ship the source bytes verbatim under their real extension — no re-encode.
    /// Use for a texture already in an optimal format, or to preserve the exact
    /// source bytes. An imported KTX2 ships verbatim (passthrough) and records
    /// the KTX2 runtime encoding.
    Source,
    /// Re-encode to lossless WebP: pixel-identical to the source, typically
    /// smaller than PNG, browser-decodable (decoded to RGBA8 on the GPU —
    /// no block compression).
    WebpLossless,
    /// Re-encode to lossy WebP at `quality` (0.0..=1.0). Smaller still, at a
    /// visible-quality cost the author accepts for this texture. Higher = larger,
    /// closer to lossless.
    WebpLossy { quality: f32 },
    /// Basis-supercompressed KTX2 (the default): one device-agnostic artifact
    /// the player transcodes to the adapter's native block format at load.
    /// Sources already in KTX2 ship verbatim regardless of `profile`.
    /// Textures whose dimensions aren't multiples of 4 fall back to lossless
    /// WebP at bake time (WebGPU block-size limit), with a log line.
    Ktx2 { profile: Ktx2Profile },
}

impl Default for TextureExport {
    fn default() -> Self {
        Self::Ktx2 {
            profile: Ktx2Profile::Auto,
        }
    }
}

/// Basis codec profile for [`TextureExport::Ktx2`].
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Ktx2Profile {
    /// Pick by material slot at bake time: UASTC for normal maps (higher
    /// quality — data maps corrupt visibly under ETC1S), ETC1S for
    /// color/roughness/metallic/emissive (much smaller).
    #[default]
    Auto,
    /// Force ETC1S (smallest).
    Etc1s,
    /// Force UASTC (highest quality).
    Uastc,
}

/// One texture USE's resolved bundle encoding — the output of
/// [`resolve_texture_use`], the bake's use-level precedence chain
/// (docs/plans/compression.md F2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResolvedTextureUse {
    /// This use wants a KTX2 artifact with these codec params. Distinct
    /// `(uastc, srgb)` pairs of the same asset become distinct variant
    /// artifacts at bake.
    Ktx2 { uastc: bool, srgb: bool },
    /// This use rides the asset-level (non-KTX2) artifact — same bytes for
    /// every such use of the asset.
    AssetLevel(TextureExport),
}

/// Resolve ONE texture use's bundle encoding:
/// **use override > per-texture pref > slot-based Auto > global**.
///
/// - `use_override` — the ref's [`TextureUseProfile`]
///   (`awsm_renderer_scene::TextureRef::export_profile`), highest precedence.
/// - `per_texture` — the asset's authored [`TextureExport`] pref.
/// - `kind` — the slot's semantic role: `Normal` picks UASTC under Auto;
///   `is_srgb()` picks the encode colorspace (per-USE, so one asset used as
///   color somewhere and data elsewhere encodes each correctly).
/// - `global` — the project [`BundleOptions`](crate::BundleOptions) texture
///   default.
pub fn resolve_texture_use(
    use_override: Option<awsm_renderer_scene::TextureUseProfile>,
    per_texture: Option<TextureExport>,
    kind: awsm_renderer_scene::TextureColorKind,
    global: crate::TextureCompression,
) -> ResolvedTextureUse {
    use awsm_renderer_scene::TextureUseProfile;
    let srgb = kind.is_srgb();
    if let Some(profile) = use_override {
        return ResolvedTextureUse::Ktx2 {
            uastc: profile == TextureUseProfile::Uastc,
            srgb,
        };
    }
    let auto = ResolvedTextureUse::Ktx2 {
        uastc: kind == awsm_renderer_scene::TextureColorKind::Normal,
        srgb,
    };
    match per_texture {
        Some(TextureExport::Ktx2 { profile }) => match profile {
            Ktx2Profile::Auto => auto,
            Ktx2Profile::Etc1s => ResolvedTextureUse::Ktx2 { uastc: false, srgb },
            Ktx2Profile::Uastc => ResolvedTextureUse::Ktx2 { uastc: true, srgb },
        },
        Some(other) => ResolvedTextureUse::AssetLevel(other),
        None => match global {
            crate::TextureCompression::Ktx2 => auto,
            crate::TextureCompression::Off => {
                ResolvedTextureUse::AssetLevel(TextureExport::WebpLossless)
            }
        },
    }
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

#[cfg(test)]
mod resolve_texture_use_tests {
    use super::*;
    use crate::TextureCompression;
    use awsm_renderer_scene::{TextureColorKind as K, TextureUseProfile};

    const KTX2_GLOBAL: TextureCompression = TextureCompression::Ktx2;

    fn ktx2(uastc: bool, srgb: bool) -> ResolvedTextureUse {
        ResolvedTextureUse::Ktx2 { uastc, srgb }
    }

    /// Slot-based Auto under the global default: Normal → UASTC/linear,
    /// color slots → ETC1S/sRGB, data slots → ETC1S/linear.
    #[test]
    fn global_auto_resolves_by_slot() {
        for (kind, want) in [
            (K::Normal, ktx2(true, false)),
            (K::Albedo, ktx2(false, true)),
            (K::Emissive, ktx2(false, true)),
            (K::MetallicRoughness, ktx2(false, false)),
            (K::Occlusion, ktx2(false, false)),
        ] {
            assert_eq!(resolve_texture_use(None, None, kind, KTX2_GLOBAL), want);
        }
    }

    /// The aliasing case the feature exists for: ONE asset used as a normal
    /// map by one material and as albedo by another resolves DIFFERENTLY per
    /// use (distinct artifacts at bake), instead of the whole asset going
    /// normal because any use was a normal slot.
    #[test]
    fn mixed_uses_resolve_independently() {
        let normal_use = resolve_texture_use(None, None, K::Normal, KTX2_GLOBAL);
        let color_use = resolve_texture_use(None, None, K::Albedo, KTX2_GLOBAL);
        assert_eq!(normal_use, ktx2(true, false));
        assert_eq!(color_use, ktx2(false, true));
        assert_ne!(normal_use, color_use);
    }

    /// Per-texture pref beats slot Auto; forced profiles keep per-use sRGB.
    #[test]
    fn per_texture_pref_beats_slot_auto() {
        let pref = Some(TextureExport::Ktx2 {
            profile: Ktx2Profile::Uastc,
        });
        assert_eq!(
            resolve_texture_use(None, pref, K::Albedo, KTX2_GLOBAL),
            ktx2(true, true),
        );
        let pref = Some(TextureExport::Ktx2 {
            profile: Ktx2Profile::Etc1s,
        });
        assert_eq!(
            resolve_texture_use(None, pref, K::Normal, KTX2_GLOBAL),
            ktx2(false, false),
        );
        // Auto pref = same as no pref (slot decides).
        let pref = Some(TextureExport::default());
        assert_eq!(
            resolve_texture_use(None, pref, K::Normal, KTX2_GLOBAL),
            ktx2(true, false),
        );
    }

    /// Use-site override beats everything, including a non-KTX2 per-texture pref.
    #[test]
    fn use_override_beats_all() {
        assert_eq!(
            resolve_texture_use(
                Some(TextureUseProfile::Uastc),
                Some(TextureExport::WebpLossless),
                K::Albedo,
                TextureCompression::Off,
            ),
            ktx2(true, true),
        );
        assert_eq!(
            resolve_texture_use(
                Some(TextureUseProfile::Etc1s),
                Some(TextureExport::Ktx2 {
                    profile: Ktx2Profile::Uastc
                }),
                K::Normal,
                KTX2_GLOBAL,
            ),
            ktx2(false, false),
        );
    }

    /// Non-KTX2 prefs and the Off global ride the asset-level path.
    #[test]
    fn asset_level_paths() {
        assert_eq!(
            resolve_texture_use(None, Some(TextureExport::Source), K::Albedo, KTX2_GLOBAL),
            ResolvedTextureUse::AssetLevel(TextureExport::Source),
        );
        assert_eq!(
            resolve_texture_use(None, None, K::Normal, TextureCompression::Off),
            ResolvedTextureUse::AssetLevel(TextureExport::WebpLossless),
        );
    }
}
