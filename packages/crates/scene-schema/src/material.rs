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
    /// Normal-map intensity (glTF `normalTexture.scale`). A per-mesh uniform —
    /// scales how strongly the normal map perturbs the surface. Only meaningful
    /// when `normal_texture` is set. Defaults to 1.0 (and round-trips for old
    /// projects via `default_one`).
    #[serde(default = "default_one")]
    pub normal_scale: f32,
    /// Ambient-occlusion mask (R channel). When set, the renderer
    /// multiplies it into the ambient/indirect lighting term.
    #[serde(default)]
    pub occlusion_texture: Option<TextureRef>,
    /// Occlusion intensity (glTF `occlusionTexture.strength`). A per-mesh uniform
    /// — lerps the AO term between 1.0 and the sampled value. Only meaningful
    /// when `occlusion_texture` is set. Defaults to 1.0.
    #[serde(default = "default_one")]
    pub occlusion_strength: f32,
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
    /// Optional KHR PBR extensions (only meaningful when `shading == Pbr`). Each
    /// enabled extension is a variant bit; its factors are per-mesh uniforms.
    /// `#[serde(default)]` so pre-extension projects round-trip cleanly.
    #[serde(default)]
    pub extensions: PbrExtensions,
}

fn default_one() -> f32 {
    1.0
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
            normal_scale: 1.0,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            double_sided: false,
            vertex_colors_enabled: false,
            alpha_mode: MaterialAlphaMode::Opaque,
            shading: MaterialShading::Pbr,
            extensions: PbrExtensions::default(),
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
    /// All five knobs map 1:1 to the renderer's `ToonMaterial`. The three added
    /// later carry serde defaults so older `Toon { diffuse_bands, rim_strength }`
    /// projects deserialize unchanged.
    Toon {
        diffuse_bands: u32,
        rim_strength: f32,
        #[serde(default = "default_specular_steps")]
        specular_steps: u32,
        #[serde(default = "default_shininess")]
        shininess: f32,
        #[serde(default = "default_rim_power")]
        rim_power: f32,
    },
}

fn default_specular_steps() -> u32 {
    2
}
fn default_shininess() -> f32 {
    32.0
}
fn default_rim_power() -> f32 {
    2.0
}

/// The KHR PBR material extensions, modeled as per-material optionals.
///
/// **Each `Some(..)` ENABLES that extension** — flipping the option is a VARIANT
/// change (it sets a `PbrFeatures` bit, so the assigned meshes recompile to a
/// distinct specialized shader; "a PBR with dispersion is a different compiled
/// material"). The scalar / color fields *inside* each struct are per-mesh
/// UNIFORM factors — editing them is a no-recompile uniform update.
///
/// Texture bindings for these extensions (specular color map, clearcoat normal,
/// …) are deliberately omitted until the texture-asset picker lands; the bridge
/// leaves those renderer slots `None`. All fields `#[serde(default)]` so existing
/// projects (which have no `extensions` key) round-trip unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct PbrExtensions {
    pub emissive_strength: Option<EmissiveStrengthExt>,
    pub ior: Option<IorExt>,
    pub specular: Option<SpecularExt>,
    pub transmission: Option<TransmissionExt>,
    pub diffuse_transmission: Option<DiffuseTransmissionExt>,
    pub volume: Option<VolumeExt>,
    pub clearcoat: Option<ClearcoatExt>,
    pub sheen: Option<SheenExt>,
    pub dispersion: Option<DispersionExt>,
    pub anisotropy: Option<AnisotropyExt>,
    pub iridescence: Option<IridescenceExt>,
}

macro_rules! ext_struct {
    ($(#[$m:meta])* $name:ident { $($field:ident : $ty:ty = $def:expr),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub struct $name { $(#[serde(default)] pub $field: $ty),* }
        impl Default for $name {
            fn default() -> Self { Self { $($field: $def),* } }
        }
    };
}

ext_struct!(/// `KHR_materials_emissive_strength` — multiplies the emissive factor.
    EmissiveStrengthExt { strength: f32 = 2.0 });
ext_struct!(/// `KHR_materials_ior` — index of refraction (1.5 ≈ glass/plastic).
    IorExt { ior: f32 = 1.5 });
ext_struct!(/// `KHR_materials_specular` — specular reflection strength + tint.
    SpecularExt { factor: f32 = 1.0, color_factor: [f32; 3] = [1.0, 1.0, 1.0],
        tex: Option<TextureRef> = None, color_tex: Option<TextureRef> = None });
ext_struct!(/// `KHR_materials_transmission` — light transmitted through the surface.
    TransmissionExt { factor: f32 = 1.0, tex: Option<TextureRef> = None });
ext_struct!(/// `KHR_materials_diffuse_transmission` — diffuse light through thin surfaces.
    DiffuseTransmissionExt { factor: f32 = 1.0, color_factor: [f32; 3] = [1.0, 1.0, 1.0],
        tex: Option<TextureRef> = None, color_tex: Option<TextureRef> = None });
ext_struct!(/// `KHR_materials_volume` — absorption inside a transmissive volume.
    VolumeExt { thickness_factor: f32 = 1.0, attenuation_distance: f32 = 1.0, attenuation_color: [f32; 3] = [1.0, 1.0, 1.0],
        thickness_tex: Option<TextureRef> = None });
ext_struct!(/// `KHR_materials_clearcoat` — a clear lacquer layer.
    ClearcoatExt { factor: f32 = 1.0, roughness_factor: f32 = 0.0, normal_scale: f32 = 1.0,
        tex: Option<TextureRef> = None, roughness_tex: Option<TextureRef> = None, normal_tex: Option<TextureRef> = None });
ext_struct!(/// `KHR_materials_sheen` — retroreflective fuzz (cloth/velvet).
    SheenExt { roughness_factor: f32 = 0.3, color_factor: [f32; 3] = [1.0, 1.0, 1.0],
        color_tex: Option<TextureRef> = None, roughness_tex: Option<TextureRef> = None });
ext_struct!(/// `KHR_materials_dispersion` — wavelength-dependent IOR (prismatic).
    DispersionExt { dispersion: f32 = 0.1 });
ext_struct!(/// `KHR_materials_anisotropy` — directional specular (brushed metal).
    AnisotropyExt { strength: f32 = 1.0, rotation: f32 = 0.0, tex: Option<TextureRef> = None });
ext_struct!(/// `KHR_materials_iridescence` — thin-film interference (soap bubble).
    IridescenceExt { factor: f32 = 1.0, ior: f32 = 1.3, thickness_min: f32 = 100.0, thickness_max: f32 = 400.0,
        tex: Option<TextureRef> = None, thickness_tex: Option<TextureRef> = None });

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
