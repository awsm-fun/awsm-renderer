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
    pub emissive: [f32; 3],
    pub double_sided: bool,
    pub vertex_colors_enabled: bool,
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
            emissive: [0.0, 0.0, 0.0],
            double_sided: false,
            vertex_colors_enabled: false,
            shading: MaterialShading::Pbr,
        }
    }
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
    /// External raster file (PNG, KTX2, etc.) located in the project's `assets/` dir.
    Raster { filename: String },
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
