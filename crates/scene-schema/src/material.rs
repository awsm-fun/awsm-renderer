//! Material asset (authorable PBR knobs + texture refs).
//!
//! `MaterialDef` lives in lockstep-game-data because it references lockstep
//! `AssetId`s for textures. The editor/player bridge resolves these to
//! `awsm-renderer::TextureKey`s at instantiation time.

use super::primitive::TextureRef;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MaterialDef {
    /// Optional human-readable label. Shown in the editor's Assets
    /// panel pill list and the asset inspector header. Empty by
    /// default (existing project.json files round-trip via
    /// `#[serde(default)]`). Plays no runtime role — purely
    /// authoring metadata.
    #[serde(default)]
    pub label: String,
    pub base_color: [f32; 4],
    #[serde(default)]
    pub base_color_texture: Option<TextureRef>,
    pub metallic: f32,
    pub roughness: f32,
    /// Combined metallic-roughness texture (glTF convention: G channel =
    /// roughness, B channel = metallic). Default-skip so existing
    /// project.json files round-trip cleanly.
    #[serde(default)]
    pub metallic_roughness_texture: Option<TextureRef>,
    pub emissive: [f32; 3],
    #[serde(default)]
    pub emissive_texture: Option<TextureRef>,
    /// Tangent-space normal map (RGB). When set, the renderer uses it to
    /// perturb the surface normal at shading time.
    #[serde(default)]
    pub normal_texture: Option<TextureRef>,
    /// Ambient-occlusion mask (R channel). When set, the renderer
    /// multiplies it into the ambient/indirect lighting term.
    #[serde(default)]
    pub occlusion_texture: Option<TextureRef>,
    pub double_sided: bool,
    pub vertex_colors_enabled: bool,
    /// glTF-style alpha rendering mode. Defaults to `Opaque` so
    /// pre-extension project.json round-trips identically (the
    /// material_to_pbr translation then falls back to the
    /// "base_color.a < 1 → blend" heuristic the editor has always used
    /// for inline procedural materials).
    #[serde(default)]
    pub alpha_mode: MaterialAlphaMode,
    /// Shading model selector. `Pbr` is the default; `Unlit` is the existing
    /// emissive-only path; `Toon` is the new banded-diffuse + stepped-specular
    /// + rim shading model added by this plan.
    pub shading: MaterialShading,
}

impl Default for MaterialDef {
    fn default() -> Self {
        Self {
            label: String::new(),
            base_color: [1.0, 1.0, 1.0, 1.0],
            base_color_texture: None,
            metallic: 0.0,
            roughness: 0.7,
            metallic_roughness_texture: None,
            emissive: [0.0, 0.0, 0.0],
            emissive_texture: None,
            normal_texture: None,
            occlusion_texture: None,
            double_sided: false,
            vertex_colors_enabled: false,
            alpha_mode: MaterialAlphaMode::Opaque,
            shading: MaterialShading::Pbr,
        }
    }
}

/// Authored alpha mode. Mirrors glTF's `material.alphaMode` so a
/// MaterialDef extracted from a glTF retains the original rendering
/// intent. `Opaque` is the default and matches the legacy
/// "base_color.a == 1 ⇒ opaque" behaviour; `Mask` carries the same
/// `alpha_cutoff` glTF stores; `Blend` enables order-dependent alpha
/// compositing.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MaterialAlphaMode {
    #[default]
    Opaque,
    Mask {
        cutoff: f32,
    },
    Blend,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Copy)]
pub enum MaterialShading {
    Pbr,
    Unlit,
    /// Banded diffuse + stepped Blinn-Phong specular + rim light.
    /// Requires the `toon` feature on `awsm-renderer` at build time.
    Toon {
        diffuse_bands: u32,
        rim_strength: f32,
    },
}

/// Procedural texture parameters. The renderer materializes these into a real
/// GPU texture at load time via `awsm-meshgen::procedural_texture::*`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProceduralTextureDef {
    Checker {
        width: u32,
        height: u32,
        cells_x: u32,
        cells_y: u32,
        color_a: [f32; 4],
        color_b: [f32; 4],
    },
    Gradient {
        width: u32,
        height: u32,
        color_a: [f32; 4],
        color_b: [f32; 4],
        horizontal: bool,
    },
    Noise {
        width: u32,
        height: u32,
        seed: u32,
        scale: f32,
    },
}

/// Texture asset variants.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureDef {
    /// External raster file (PNG, JPEG, WebP) shipped alongside the
    /// project. `display_name` is the user-facing label + provides the
    /// extension for the on-disk file; the disk path itself is derived
    /// from the entry's `content_hash` (see
    /// `AssetEntry::content_hash`), not from this string.
    Raster { display_name: String },
    /// Procedurally generated at load time.
    Procedural(ProceduralTextureDef),
}

/// Metadata for a captured procedural mesh stored as a side file.
///
/// The actual geometry bytes live at `assets/<asset-id>.mesh.bin`
/// alongside the project (see [`mesh_asset_filename`] +
/// [`CapturedMesh`]). Keeping the bytes out of `project.json` means
/// the JSON stays small even when many meshes are captured. Conversion
/// to/from the in-memory representation is done by the renderer-bridge
/// / scene-build at materialize time.
///
/// `source` records the kind the mesh was captured from. The editor's
/// Mesh inspector renders editable copies of those params; mutating
/// them auto-regenerates the bytes against the same AssetId, so every
/// referencing `NodeKind::Mesh` picks up the change without the user
/// having to find a source node in the tree.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MeshDef {
    pub label: String,
    #[serde(default)]
    pub source: Option<CapturedSource>,
}

/// Where a captured mesh's geometry came from. Stored on `MeshDef`
/// so the Mesh inspector can render the source params + re-capture
/// without a separate source node.
///
/// `Sweep`'s `curve_node` is a `NodeId` reference into the live
/// scene; if that node is deleted between captures the inspector
/// falls back to the legacy "pick a source from scene" picker.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturedSource {
    Primitive(super::primitive::PrimitiveShape),
    Sweep(super::instances::SweepAlongCurveDef),
}

/// Captured procedural-mesh geometry, bitcode-serialized into the
/// project's `assets/<asset-id>.mesh.bin` side file. Mirrors the
/// in-memory shape of `awsm_meshgen::MeshData` so the materializer can
/// hand the data straight to the renderer without massaging.
///
/// `lockstep-game-data` doesn't depend on `awsm-meshgen` (it's a
/// renderer-side crate), so this is a local mirror. Conversion helpers
/// live in the consuming crates (`game-editor` + `game-player`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CapturedMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Option<Vec<[f32; 3]>>,
    pub uvs: Option<Vec<[f32; 2]>>,
    pub colors: Option<Vec<[f32; 4]>>,
    pub indices: Vec<u32>,
}

/// File extension for captured-mesh side files — `<asset-id>.mesh.bin`.
pub const MESH_FILE_EXTENSION: &str = "mesh.bin";

/// On-disk filename for the captured-mesh bytes of an
/// `AssetSource::Mesh` entry. Mirrors how `AssetSource::Filename`
/// computes `assets/<name>` — here the name is derived from the
/// asset's UUID so each captured mesh gets a stable, collision-free
/// file. Returns just the leaf filename; callers prepend `assets/`
/// (or whatever directory layout they use).
pub fn mesh_asset_filename(asset_id: super::assets::AssetId) -> String {
    format!("{}.{}", asset_id.0, MESH_FILE_EXTENSION)
}
