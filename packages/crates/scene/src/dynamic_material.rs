//! On-disk shape of a runtime-registered "custom" material.
//!
//! A custom material is a **folder** (not a single file). The folder contains:
//!
//! ```text
//! my-material/
//! ├── material.json          // [`MaterialDefinition`] serialized as JSON
//! ├── shader.wgsl            // the author's WGSL fragment (always this name)
//! └── assets/
//!     ├── *.png / *.ktx2     // textures referenced by [`TextureSlot::default`]
//!     └── *.bin              // raw u32 buffer data for [`BufferSlot::default`]
//! ```
//!
//! `material-editor` exports folders in this shape; `scene-editor` imports
//! them under `<project>/assets/materials/<name>/` and references them via
//! [`CustomMaterialRef`] on the project root.
//!
//! The renderer reads [`LoadedMaterialFolder`] (the file-system-resolved
//! variant); both editors and any third-party scene player share this same
//! loader. The renderer itself does NOT depend on `awsm-renderer-scene` — the
//! consumer converts the loaded folder into
//! `awsm_renderer::dynamic_materials::MaterialRegistration` before calling
//! `AwsmRenderer::register_material`.

use std::collections::HashMap;
use std::path::PathBuf;

use thiserror::Error;

use crate::assets::AssetId;
use crate::material::{MaterialAlphaMode, MaterialDef};

const DEFAULT_VERSION: u32 = 1;

fn default_version() -> u32 {
    DEFAULT_VERSION
}

/// On-disk shape of a custom material.
///
/// Lives in `material.json` at the root of a material folder. Companion
/// `shader.wgsl` and any referenced texture / buffer assets are loaded
/// separately by `load_material_folder` (behind the `fs-loader` feature) into a [`LoadedMaterialFolder`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MaterialDefinition {
    /// Author-facing name. Must be a valid folder name (kebab-case
    /// recommended; the editor enforces it). Matches the parent folder's
    /// name; the loader cross-checks them.
    pub name: String,
    /// Author-bumped layout-version counter. Materials with the same name
    /// but a different `version` are treated as distinct registrations —
    /// the typical case where the author makes a breaking layout change
    /// (e.g. reordered uniforms) and wants old projects referencing the
    /// old version to fail-load rather than silently rebind.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Material alpha mode. Drives whether the renderer routes the
    /// material through the opaque compute kernel
    /// ([`MaterialAlphaMode::Opaque`]) or the transparent fragment shader
    /// ([`MaterialAlphaMode::Mask`] / [`MaterialAlphaMode::Blend`]).
    #[serde(default)]
    pub alpha_mode: MaterialAlphaMode,
    /// Whether the material renders both front- and back-facing
    /// triangles.
    #[serde(default)]
    pub double_sided: bool,
    /// Per-material uniform parameters. Become fields in the auto-generated
    /// `MaterialData` WGSL struct (in declaration order, respecting WGSL
    /// alignment).
    #[serde(default)]
    pub uniforms: Vec<UniformField>,
    /// Texture slots the author samples in their WGSL fragment. Each becomes
    /// a `<name>_index: u32` field in the auto-generated `MaterialData`
    /// struct.
    #[serde(default)]
    pub textures: Vec<TextureSlot>,
    /// Variable-length per-material buffer slots. Each becomes a
    /// `<name>_offset: u32` + `<name>_length: u32` pair in the
    /// auto-generated `MaterialData` struct; the data lives in the
    /// renderer-wide extras pool.
    #[serde(default)]
    pub buffers: Vec<BufferSlot>,
    /// Renderer shader-include keys the author opted into (the runtime
    /// `MaterialRegistration` needs these to assemble the shader). `#[serde(default)]`
    /// so pre-existing material.json files round-trip as "none opted in".
    #[serde(default)]
    pub shader_includes: Vec<String>,
    /// Fragment-input keys the author opted into (passed to the runtime
    /// `MaterialRegistration`). `#[serde(default)]` for back-compat.
    #[serde(default)]
    pub fragment_inputs: Vec<String>,
}

/// A single uniform parameter on a [`MaterialDefinition`].
///
/// The `name` becomes the field name in the auto-generated WGSL
/// `MaterialData` struct. `ty` picks the WGSL field type; `default`
/// supplies the value used when no per-instance override is provided.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UniformField {
    /// Field name. Becomes the field name in the WGSL `MaterialData`
    /// struct.
    pub name: String,
    /// WGSL type.
    pub ty: FieldType,
    /// Default value used at instance time when no
    /// [`MaterialInstance::uniform_overrides`] entry exists.
    pub default: UniformValue,
}

/// WGSL field type supported by [`UniformField`].
///
/// Numbers reflect the ordering used by the on-disk JSON tag — keep this
/// stable across releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// `f32`
    F32,
    /// `vec2<f32>`
    Vec2,
    /// `vec3<f32>` — 16-byte aligned, 12 bytes payload + 4 bytes padding.
    Vec3,
    /// `vec4<f32>`
    Vec4,
    /// `u32`
    U32,
    /// `vec2<i32>`
    IVec2,
    /// `vec3<i32>`
    IVec3,
    /// `vec4<i32>`
    IVec4,
    /// `mat3x3<f32>` — 16-byte aligned, 48 bytes payload.
    Mat3,
    /// `mat4x4<f32>`
    Mat4,
    /// `vec3<f32>` with a color-picker UI in `material-editor`.
    Color3,
    /// `vec4<f32>` with a color-picker UI in `material-editor`.
    Color4,
    /// Becomes a `u32` (0 or 1) in WGSL; rendered as a checkbox in
    /// `material-editor`.
    Bool,
}

/// Default value for a [`UniformField`], and the in-memory shape of per-
/// instance overrides on [`MaterialInstance::uniform_overrides`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum UniformValue {
    /// `f32`
    F32(f32),
    /// `vec2<f32>`
    Vec2([f32; 2]),
    /// `vec3<f32>`
    Vec3([f32; 3]),
    /// `vec4<f32>`
    Vec4([f32; 4]),
    /// `u32`
    U32(u32),
    /// `vec2<i32>`
    IVec2([i32; 2]),
    /// `vec3<i32>`
    IVec3([i32; 3]),
    /// `vec4<i32>`
    IVec4([i32; 4]),
    /// `mat3x3<f32>` packed as 9 column-major f32s.
    Mat3([f32; 9]),
    /// `mat4x4<f32>` packed as 16 column-major f32s.
    Mat4([f32; 16]),
    /// 3-channel color.
    Color3([f32; 3]),
    /// 4-channel color (RGBA).
    Color4([f32; 4]),
    /// Becomes a `u32` (0 or 1) in WGSL.
    Bool(bool),
}

impl UniformValue {
    /// Returns the [`FieldType`] this value satisfies. Used to verify a
    /// per-instance override matches the layout's declared type.
    pub fn field_type(&self) -> FieldType {
        match self {
            UniformValue::F32(_) => FieldType::F32,
            UniformValue::Vec2(_) => FieldType::Vec2,
            UniformValue::Vec3(_) => FieldType::Vec3,
            UniformValue::Vec4(_) => FieldType::Vec4,
            UniformValue::U32(_) => FieldType::U32,
            UniformValue::IVec2(_) => FieldType::IVec2,
            UniformValue::IVec3(_) => FieldType::IVec3,
            UniformValue::IVec4(_) => FieldType::IVec4,
            UniformValue::Mat3(_) => FieldType::Mat3,
            UniformValue::Mat4(_) => FieldType::Mat4,
            UniformValue::Color3(_) => FieldType::Color3,
            UniformValue::Color4(_) => FieldType::Color4,
            UniformValue::Bool(_) => FieldType::Bool,
        }
    }
}

/// A texture slot on a [`MaterialDefinition`].
///
/// The `name` becomes `<name>_index: u32` in the auto-generated WGSL
/// `MaterialData` struct. The author samples the texture via the existing
/// texture-pool helpers using that index.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TextureSlot {
    /// Slot name. Becomes `<name>_index: u32` in the WGSL `MaterialData`
    /// struct.
    pub name: String,
    /// Path relative to the material folder root (typically inside
    /// `assets/`). Optional — slots without a default require a binding at
    /// instance time (via
    /// [`MaterialInstance::texture_overrides`]).
    #[serde(default)]
    pub default: Option<PathBuf>,
}

/// A variable-length per-material buffer slot on a [`MaterialDefinition`].
///
/// The `name` becomes `<name>_offset: u32` and `<name>_length: u32` in the
/// auto-generated WGSL `MaterialData` struct. The author reads the data via
/// the renderer-side `extras_load_f32` / `extras_load_u32` helpers (see
/// `shared_wgsl/extras.wgsl`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BufferSlot {
    /// Slot name. Becomes `<name>_offset: u32` + `<name>_length: u32` in
    /// the WGSL `MaterialData` struct.
    pub name: String,
    /// Path to a `.bin` file (raw little-endian u32 words) relative to the
    /// material folder root, typically inside `assets/`. Optional — slots
    /// without a default require a binding at instance time (via
    /// [`MaterialInstance::buffer_overrides`]).
    ///
    /// The file size must be a multiple of 4. The loader returns
    /// [`MaterialFolderError::BinSizeNotMultipleOfFour`] otherwise.
    #[serde(default)]
    pub default: Option<PathBuf>,
}

/// A project-root pointer to a custom material folder.
///
/// Lives in `EditorProject::custom_materials`. The folder is copied into
/// `<project>/assets/materials/<name>/` on import; the `folder` field is
/// project-relative.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CustomMaterialRef {
    /// Stable id of the custom material — the same [`AssetId`] a node's
    /// [`MaterialInstance::asset`] carries, so the player can map an assignment
    /// to this entry (and its folder). `#[serde(default)]` so pre-id bundles
    /// still load (the player then can't resolve custom assignments).
    #[serde(default)]
    pub id: AssetId,
    /// Folder name. Matches the parent folder's name AND the
    /// [`MaterialDefinition::name`] inside the folder's `material.json`.
    pub name: String,
    /// Project-relative folder path (e.g. `assets/materials/scanline`).
    pub folder: PathBuf,
}

/// Per-geometry-node material assignment — the single material field carried
/// by every renderable node (Primitive / Mesh / SweepAlongCurve / Model).
///
/// `asset` is the stable id of the assigned material in the editor's
/// custom-material list. That entry may be a **built-in** material (PBR /
/// Unlit / Toon, glTF-representable) or a **custom WGSL** material:
///
/// - For a built-in assignment, the per-mesh uniform values (base color /
///   metallic / roughness / emissive / extension params + textures) live in
///   [`MaterialInstance::inline`]; the override maps are ignored.
/// - For a custom-WGSL assignment, the per-mesh overrides live in the
///   `uniform_overrides` / `texture_overrides` / `buffer_overrides` maps
///   (resolved against the renderer's `MaterialRegistry` at bridge time);
///   `inline` is ignored.
///
/// A `None` material on a node means *unassigned* and renders flat magenta —
/// the missing-material sentinel.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MaterialInstance {
    /// Stable id of the assigned material (built-in OR custom WGSL), an entry
    /// in the editor's custom-material list. Id-keyed (not name-keyed) so
    /// renaming a material never orphans the meshes assigned to it.
    pub asset: AssetId,
    /// Per-mesh built-in uniform values (base_color / metallic / roughness /
    /// emissive / extension params + textures). Used when `asset` resolves to
    /// a BUILT-IN material; IGNORED by custom-WGSL assignments.
    #[serde(default)]
    pub inline: MaterialDef,
    /// Per-instance uniform overrides for a CUSTOM-WGSL assignment. Keys must
    /// match a [`UniformField::name`] on the registered material's layout;
    /// values must satisfy the corresponding [`FieldType`]. Ignored by
    /// built-in assignments.
    #[serde(default)]
    pub uniform_overrides: HashMap<String, UniformValue>,
    /// Per-instance texture overrides. Keys must match a
    /// [`TextureSlot::name`].
    #[serde(default)]
    pub texture_overrides: HashMap<String, crate::primitive::TextureRef>,
    /// Per-instance buffer overrides. Keys must match a
    /// [`BufferSlot::name`].
    #[serde(default)]
    pub buffer_overrides: HashMap<String, BufferRef>,
}

/// Pointer to a buffer-data asset bound into a custom-material buffer slot.
/// Mirrors [`crate::primitive::TextureRef`]: it carries a durable `AssetId`, not
/// a path. The bytes (raw little-endian `u32` words) are content-addressed like a
/// raster texture — the editor caches them and persists `assets/<content_hash>.bin`;
/// the player fetches `assets/<asset>.bin`. (Earlier builds stored a transient
/// `session://buffer/<id>` path here, which didn't survive a project reload.)
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BufferRef {
    /// Stable id of the buffer-data asset bound to this slot.
    pub asset: AssetId,
}

impl BufferRef {
    pub fn new(asset: AssetId) -> Self {
        Self { asset }
    }
}

impl From<AssetId> for BufferRef {
    fn from(asset: AssetId) -> Self {
        Self::new(asset)
    }
}

/// Resolved, in-memory representation of a custom-material folder.
///
/// The output of `load_material_folder` (behind the `fs-loader` feature). Consumers (`scene-editor`,
/// `material-editor`, any third-party scene player) convert this to a
/// renderer-side `MaterialRegistration` before calling
/// `AwsmRenderer::register_material`.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedMaterialFolder {
    /// Parsed `material.json`.
    pub definition: MaterialDefinition,
    /// Raw bytes of `shader.wgsl`.
    pub wgsl_source: String,
    /// Resolved bytes for every [`TextureSlot::default`] referenced.
    /// Keyed by the slot's `default` path (relative to the material
    /// folder root).
    pub texture_data: HashMap<PathBuf, Vec<u8>>,
    /// Resolved bytes for every [`BufferSlot::default`] referenced.
    /// Keyed by the slot's `default` path. Each `Vec<u32>` is a
    /// little-endian decoded view of the file's bytes; the file size
    /// has already been validated as a multiple of 4.
    pub buffer_data: HashMap<PathBuf, Vec<u32>>,
}

/// Errors produced by `load_material_folder` (behind the `fs-loader` feature).
#[derive(Error, Debug)]
pub enum MaterialFolderError {
    /// The folder did not contain a `material.json`, or it could not be
    /// opened.
    #[error("material.json missing or unreadable at {path:?}: {source}")]
    MaterialJsonMissing {
        /// Path the loader tried to read.
        path: PathBuf,
        /// Wrapped IO error.
        #[source]
        source: std::io::Error,
    },

    /// `material.json` was present but did not parse as a
    /// [`MaterialDefinition`].
    ///
    /// The `message` is the formatted serde error (line / column when
    /// available) — kept as a `String` rather than `serde_json::Error`
    /// so the variant stays portable when the `fs-loader` feature is
    /// disabled.
    #[error("material.json at {path:?} failed to parse: {message}")]
    MaterialJsonParse {
        /// Path that failed to parse.
        path: PathBuf,
        /// Formatted serde error.
        message: String,
    },

    /// The folder did not contain a `shader.wgsl`, or it could not be
    /// opened.
    #[error("shader.wgsl missing or unreadable at {path:?}: {source}")]
    ShaderMissing {
        /// Path the loader tried to read.
        path: PathBuf,
        /// Wrapped IO error.
        #[source]
        source: std::io::Error,
    },

    /// A [`TextureSlot::default`] pointed to a file that didn't exist or
    /// couldn't be read.
    #[error("texture asset missing at {path:?}: {source}")]
    TextureAssetMissing {
        /// Path the loader tried to read.
        path: PathBuf,
        /// Wrapped IO error.
        #[source]
        source: std::io::Error,
    },

    /// A [`BufferSlot::default`] pointed to a file that didn't exist or
    /// couldn't be read.
    #[error("buffer asset missing at {path:?}: {source}")]
    BufferAssetMissing {
        /// Path the loader tried to read.
        path: PathBuf,
        /// Wrapped IO error.
        #[source]
        source: std::io::Error,
    },

    /// A `.bin` file's length on disk was not a multiple of 4 bytes —
    /// the extras pool can only hold whole u32 words.
    #[error("buffer asset at {path:?} has {byte_len} bytes, not a multiple of 4")]
    BinSizeNotMultipleOfFour {
        /// Offending file path.
        path: PathBuf,
        /// File length in bytes.
        byte_len: usize,
    },

    /// Two layout entries (uniforms / textures / buffers) shared the same
    /// `name`.
    #[error("layout name collision: `{0}` is declared more than once")]
    NameCollision(String),

    /// A layout entry's `name` collides with a kernel-provided WGSL
    /// symbol — these symbols are pre-declared by the renderer's template
    /// substitution and an author's `<name>_index` / `<name>_offset` /
    /// uniform field would shadow them.
    ///
    /// Reserved names: `material`, `texture_pool`, `extras_pool`,
    /// `frame_globals`, `camera`, `frag`, `vert`.
    #[error("layout entry uses reserved name `{0}` (collides with kernel-provided symbol)")]
    ReservedName(String),

    /// The folder's parent-directory name did not match
    /// [`MaterialDefinition::name`].
    #[error("folder name `{folder}` does not match material.json name `{material}`")]
    FolderNameMismatch {
        /// On-disk folder name.
        folder: String,
        /// `material.json::name` value.
        material: String,
    },
}

/// Names the renderer reserves for kernel-provided symbols — an author's
/// layout field cannot use any of these.
///
/// Kept here (not in the renderer) because the loader produces the error;
/// the renderer side enforces the same list when the substitution emits
/// the auto-generated struct.
pub const RESERVED_LAYOUT_NAMES: &[&str] = &[
    "material",
    "texture_pool",
    "extras_pool",
    "frame_globals",
    "camera",
    "frag",
    "vert",
];

/// Loads a material folder from disk and produces a
/// [`LoadedMaterialFolder`].
///
/// `root` is the path to the folder (e.g.
/// `<project>/assets/materials/scanline`). The folder must contain
/// `material.json` and `shader.wgsl`; every [`TextureSlot::default`] /
/// [`BufferSlot::default`] path is resolved relative to `root` and its
/// bytes are read into the returned struct.
///
/// Cross-checks:
/// - `material.json::name` matches the folder name.
/// - No two layout entries share a `name`.
/// - No layout entry uses a name in [`RESERVED_LAYOUT_NAMES`].
/// - Every `.bin` file's size is a multiple of 4.
#[cfg(feature = "fs-loader")]
pub fn load_material_folder(
    root: &std::path::Path,
) -> Result<LoadedMaterialFolder, MaterialFolderError> {
    use std::fs;

    // 1. Read + parse material.json.
    let material_json_path = root.join("material.json");
    let material_json = fs::read_to_string(&material_json_path).map_err(|source| {
        MaterialFolderError::MaterialJsonMissing {
            path: material_json_path.clone(),
            source,
        }
    })?;
    let definition: MaterialDefinition =
        serde_json::from_str(&material_json).map_err(|source| {
            MaterialFolderError::MaterialJsonParse {
                path: material_json_path,
                message: source.to_string(),
            }
        })?;

    // 2. Cross-check folder name matches the material name. We only
    //    enforce this when the parent path actually has a file name —
    //    a loader called with `/` or a temp dir whose name is
    //    intentionally synthetic shouldn't bork.
    if let Some(folder_name) = root.file_name().and_then(|s| s.to_str()) {
        if !folder_name.is_empty() && folder_name != definition.name {
            return Err(MaterialFolderError::FolderNameMismatch {
                folder: folder_name.to_string(),
                material: definition.name.clone(),
            });
        }
    }

    // 3. Name-collision + reserved-name checks across uniforms /
    //    textures / buffers.
    validate_layout_names(&definition)?;

    // 4. Read shader.wgsl.
    let shader_path = root.join("shader.wgsl");
    let wgsl_source =
        fs::read_to_string(&shader_path).map_err(|source| MaterialFolderError::ShaderMissing {
            path: shader_path,
            source,
        })?;

    // 5. Resolve every texture default.
    let mut texture_data = HashMap::new();
    for slot in &definition.textures {
        if let Some(default) = &slot.default {
            let full_path = root.join(default);
            let bytes = fs::read(&full_path).map_err(|source| {
                MaterialFolderError::TextureAssetMissing {
                    path: full_path,
                    source,
                }
            })?;
            texture_data.insert(default.clone(), bytes);
        }
    }

    // 6. Resolve every buffer default — read raw bytes, validate
    //    multiple-of-4, decode into a `Vec<u32>`.
    let mut buffer_data = HashMap::new();
    for slot in &definition.buffers {
        if let Some(default) = &slot.default {
            let full_path = root.join(default);
            let bytes =
                fs::read(&full_path).map_err(|source| MaterialFolderError::BufferAssetMissing {
                    path: full_path.clone(),
                    source,
                })?;
            if bytes.len() % 4 != 0 {
                return Err(MaterialFolderError::BinSizeNotMultipleOfFour {
                    path: full_path,
                    byte_len: bytes.len(),
                });
            }
            let words = decode_bin_words(&bytes);
            buffer_data.insert(default.clone(), words);
        }
    }

    Ok(LoadedMaterialFolder {
        definition,
        wgsl_source,
        texture_data,
        buffer_data,
    })
}

/// Validates that no two layout entries share a name and that no entry
/// uses one of the [`RESERVED_LAYOUT_NAMES`].
///
/// Exposed alongside `load_material_folder` (behind the `fs-loader` feature) so non-native consumers
/// (the browser-side `material-editor`) can validate a
/// [`MaterialDefinition`] that was assembled in memory rather than read
/// from disk.
pub fn validate_layout_names(definition: &MaterialDefinition) -> Result<(), MaterialFolderError> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for name in definition
        .uniforms
        .iter()
        .map(|f| f.name.as_str())
        .chain(definition.textures.iter().map(|t| t.name.as_str()))
        .chain(definition.buffers.iter().map(|b| b.name.as_str()))
    {
        if RESERVED_LAYOUT_NAMES.contains(&name) {
            return Err(MaterialFolderError::ReservedName(name.to_string()));
        }
        if !seen.insert(name) {
            return Err(MaterialFolderError::NameCollision(name.to_string()));
        }
    }
    Ok(())
}

/// Decodes a `.bin` file's raw bytes into a `Vec<u32>`. Caller has
/// already verified `bytes.len() % 4 == 0`.
///
/// Reads the file as little-endian u32 words — the convention is
/// platform-agnostic so material folders are portable across hosts. The
/// renderer's WGSL `extras_load_u32` / `extras_load_f32` helpers read the
/// same words back via `bitcast`.
pub fn decode_bin_words(bytes: &[u8]) -> Vec<u32> {
    debug_assert!(bytes.len() % 4 == 0);
    bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_def() -> MaterialDefinition {
        MaterialDefinition {
            name: "scanline".to_string(),
            version: 1,
            alpha_mode: MaterialAlphaMode::Opaque,
            double_sided: false,
            uniforms: vec![
                UniformField {
                    name: "tint".into(),
                    ty: FieldType::Color3,
                    default: UniformValue::Color3([0.6, 0.9, 0.6]),
                },
                UniformField {
                    name: "scan_freq".into(),
                    ty: FieldType::F32,
                    default: UniformValue::F32(80.0),
                },
            ],
            textures: vec![TextureSlot {
                name: "base".into(),
                default: Some(PathBuf::from("assets/base.png")),
            }],
            buffers: vec![BufferSlot {
                name: "frames".into(),
                default: Some(PathBuf::from("assets/frames.bin")),
            }],
            shader_includes: vec!["camera".into()],
            fragment_inputs: vec!["world_normal".into()],
        }
    }

    #[test]
    fn definition_json_round_trip() {
        let def = sample_def();
        let json = serde_json::to_string(&def).unwrap();
        let back: MaterialDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn uniform_value_field_type_consistency() {
        for value in [
            UniformValue::F32(1.0),
            UniformValue::Vec2([0.0, 0.0]),
            UniformValue::Vec3([0.0, 0.0, 0.0]),
            UniformValue::Vec4([0.0; 4]),
            UniformValue::U32(0),
            UniformValue::IVec2([0, 0]),
            UniformValue::IVec3([0, 0, 0]),
            UniformValue::IVec4([0; 4]),
            UniformValue::Mat3([0.0; 9]),
            UniformValue::Mat4([0.0; 16]),
            UniformValue::Color3([0.0; 3]),
            UniformValue::Color4([0.0; 4]),
            UniformValue::Bool(false),
        ] {
            let ty = value.field_type();
            // round-trip through json — a smoke test that the serde tags
            // stay in lockstep across both halves.
            let json = serde_json::to_string(&value).unwrap();
            let back: UniformValue = serde_json::from_str(&json).unwrap();
            assert_eq!(back.field_type(), ty);
        }
    }

    #[test]
    fn reserved_name_rejected() {
        let mut def = sample_def();
        def.uniforms.push(UniformField {
            name: "extras_pool".into(),
            ty: FieldType::F32,
            default: UniformValue::F32(0.0),
        });
        let err = validate_layout_names(&def).unwrap_err();
        match err {
            MaterialFolderError::ReservedName(name) => assert_eq!(name, "extras_pool"),
            other => panic!("expected ReservedName, got {other:?}"),
        }
    }

    #[test]
    fn name_collision_rejected() {
        let mut def = sample_def();
        // texture "tint" collides with the existing uniform "tint"
        def.textures.push(TextureSlot {
            name: "tint".into(),
            default: None,
        });
        let err = validate_layout_names(&def).unwrap_err();
        match err {
            MaterialFolderError::NameCollision(name) => assert_eq!(name, "tint"),
            other => panic!("expected NameCollision, got {other:?}"),
        }
    }

    #[test]
    fn decode_bin_words_little_endian() {
        let bytes = [0x01, 0x02, 0x03, 0x04, 0xff, 0xff, 0xff, 0xff];
        let words = decode_bin_words(&bytes);
        assert_eq!(words, vec![0x0403_0201, 0xffff_ffff]);
    }

    /// Parse the canonical test-material `material.json` files
    /// shipped under `assets/test-materials/`. These are the
    /// procedural placeholders driving the dynamic-material end-to-end
    /// surface (irregular-atlas + soft-glass + scanline). A change
    /// to the schema serde tags that breaks the on-disk JSON would
    /// otherwise only surface at first-use in an editor — this test
    /// catches it in CI.
    #[test]
    fn test_material_json_files_parse() {
        // Anchor on `CARGO_MANIFEST_DIR` (the crate root) instead of
        // `current_dir()` so the test is invariant to the runner's
        // working directory — `cargo test`, IDE-driven runs, and CI
        // harnesses can all set cwd differently. Walk ancestors until
        // we find the workspace root (the directory containing
        // `assets/test-materials/`).
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .ancestors()
            .find(|p| p.join("assets/test-materials").is_dir())
            .unwrap_or_else(|| {
                panic!(
                    "could not locate workspace root (no assets/test-materials/ \
                     ancestor of {manifest_dir:?})"
                )
            });
        for folder in ["scanline", "irregular-atlas", "soft-glass"] {
            let path = workspace_root.join(format!("assets/test-materials/{folder}/material.json"));
            let text =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
            let def: MaterialDefinition =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"));
            validate_layout_names(&def).unwrap_or_else(|e| panic!("validate {path:?}: {e:?}"));
            assert_eq!(def.name, folder.to_string());
        }
    }

    #[cfg(feature = "fs-loader")]
    #[test]
    fn loader_round_trip() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!("awsm-scanline-test-{}", uuid::Uuid::new_v4()));
        // The folder name must match the material name in our sample.
        let folder = tmp.join("scanline");
        fs::create_dir_all(folder.join("assets")).unwrap();

        let def = sample_def();
        fs::write(
            folder.join("material.json"),
            serde_json::to_string_pretty(&def).unwrap(),
        )
        .unwrap();
        fs::write(folder.join("shader.wgsl"), b"// stub").unwrap();
        // base.png — any bytes work; the loader stores them raw.
        fs::write(folder.join("assets/base.png"), b"PNG-BYTES").unwrap();
        // frames.bin — 8 bytes = two u32 words.
        fs::write(folder.join("assets/frames.bin"), [1u8, 0, 0, 0, 2, 0, 0, 0]).unwrap();

        let loaded = load_material_folder(&folder).unwrap();
        assert_eq!(loaded.definition, def);
        assert_eq!(loaded.wgsl_source, "// stub");
        assert_eq!(
            loaded.texture_data.get(&PathBuf::from("assets/base.png")),
            Some(&b"PNG-BYTES".to_vec())
        );
        assert_eq!(
            loaded.buffer_data.get(&PathBuf::from("assets/frames.bin")),
            Some(&vec![1u32, 2u32])
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[cfg(feature = "fs-loader")]
    #[test]
    fn loader_bin_size_not_multiple_of_four_rejected() {
        use std::fs;
        let tmp =
            std::env::temp_dir().join(format!("awsm-scanline-bin-test-{}", uuid::Uuid::new_v4()));
        let folder = tmp.join("scanline");
        fs::create_dir_all(folder.join("assets")).unwrap();

        let mut def = sample_def();
        def.textures.clear();
        // Keep just a single buffer slot with a malformed .bin
        def.buffers = vec![BufferSlot {
            name: "frames".into(),
            default: Some(PathBuf::from("assets/bad.bin")),
        }];
        fs::write(
            folder.join("material.json"),
            serde_json::to_string_pretty(&def).unwrap(),
        )
        .unwrap();
        fs::write(folder.join("shader.wgsl"), b"// stub").unwrap();
        fs::write(folder.join("assets/bad.bin"), b"\x01\x02\x03").unwrap(); // 3 bytes

        let err = load_material_folder(&folder).unwrap_err();
        match err {
            MaterialFolderError::BinSizeNotMultipleOfFour { byte_len, .. } => {
                assert_eq!(byte_len, 3);
            }
            other => panic!("expected BinSizeNotMultipleOfFour, got {other:?}"),
        }

        fs::remove_dir_all(&tmp).ok();
    }

    // Helper: keep the unused-import lint quiet when `fs-loader` is off.
    #[allow(dead_code)]
    fn _ensure_hashmap_used() -> HashMap<String, UniformValue> {
        HashMap::new()
    }
}
