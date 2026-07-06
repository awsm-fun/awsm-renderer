//! Material asset (authorable PBR knobs + texture refs).
//!
//! `MaterialDef` lives in lockstep-game-data because it references lockstep
//! `AssetId`s for textures. The editor/player bridge resolves these to
//! `awsm-renderer::TextureKey`s at instantiation time.

use super::primitive::TextureRef;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    /// glTF-style alpha rendering mode. Defaults to `Opaque`, which — per
    /// glTF — IGNORES the base-color alpha factor. Transparency requires
    /// explicitly authoring `Blend`; cutouts require `Mask`. The alpha mode
    /// is pipeline ROUTING and therefore owned by the material asset alone
    /// (never a per-node override).
    #[serde(default)]
    pub alpha_mode: MaterialAlphaMode,
    /// Shading model selector. `Pbr` is the default; `Unlit` is the existing
    /// emissive-only path; `Toon` is the new banded-diffuse + stepped-specular
    /// + rim shading model added by this plan.
    pub shading: MaterialShading,
    /// Optional KHR PBR extensions (only meaningful when `shading == Pbr`). Each
    /// enabled extension is a variant bit; its factors are per-mesh uniforms.
    /// `#[serde(default)]` so pre-extension projects round-trip cleanly.
    ///
    /// Extensions are STRICT capabilities: enabling one on the material means
    /// every mesh using this material runs its code unconditionally (nodes
    /// override only the parameter uniforms). A mesh that shouldn't have the
    /// extension uses a different material.
    #[serde(default)]
    pub extensions: PbrExtensions,
    /// Texture-slot CAPABILITIES — which of the five standard PBR slots this
    /// material's compiled shader is ABLE to sample. Declaring a capability
    /// compiles the slot's sampling path in (guarded by a cheap per-material
    /// runtime "bound?" check), so meshes sharing this material can each bind
    /// or omit an image without minting a new pipeline. A slot that is neither
    /// declared here nor bound on the material itself compiles NO code and
    /// per-node binds to it are rejected.
    ///
    /// `None` (the default, and every pre-capability project) derives the
    /// capabilities from texture presence — byte-identical pipelines to the
    /// old behavior. Binding a texture on the material always implies the
    /// capability; this field can only WIDEN, never narrow. See
    /// [`MaterialDef::slot_capabilities`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture_capabilities: Option<TextureCapabilities>,
}

/// Which of the five standard PBR texture slots a material's shader can
/// sample — the compile-time half of the capability/usage split (usage is the
/// per-mesh binding). See [`MaterialDef::texture_capabilities`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TextureCapabilities {
    #[serde(default)]
    pub base_color: bool,
    #[serde(default)]
    pub metallic_roughness: bool,
    #[serde(default)]
    pub normal: bool,
    #[serde(default)]
    pub occlusion: bool,
    #[serde(default)]
    pub emissive: bool,
}

fn default_one() -> f32 {
    1.0
}

impl MaterialDef {
    /// Every texture-asset reference this material carries — the five standard
    /// PBR slots plus every enabled extension's slots (the extension side is
    /// macro-generated per field via [`CollectTextureRefs`], so a new `tex`
    /// field in an `ext_struct!` is enumerated automatically). Loaders use this
    /// to prefetch/decode a scene's unique images concurrently before the
    /// material walk binds them.
    pub fn texture_refs(&self) -> Vec<&TextureRef> {
        let mut out = Vec::new();
        for slot in [
            &self.base_color_texture,
            &self.metallic_roughness_texture,
            &self.emissive_texture,
            &self.normal_texture,
            &self.occlusion_texture,
        ] {
            slot.collect_texture_refs(&mut out);
        }
        self.extensions.texture_refs(&mut out);
        out
    }

    /// EFFECTIVE slot capabilities: the declared [`Self::texture_capabilities`]
    /// widened by the material's own bound textures (a bound default image
    /// always implies the capability). This — not raw texture presence — is
    /// what keys the compiled shader's feature set, so meshes sharing this
    /// material share one pipeline regardless of which of them bind images.
    pub fn slot_capabilities(&self) -> TextureCapabilities {
        let declared = self.texture_capabilities.unwrap_or_default();
        TextureCapabilities {
            base_color: declared.base_color || self.base_color_texture.is_some(),
            metallic_roughness: declared.metallic_roughness
                || self.metallic_roughness_texture.is_some(),
            normal: declared.normal || self.normal_texture.is_some(),
            occlusion: declared.occlusion || self.occlusion_texture.is_some(),
            emissive: declared.emissive || self.emissive_texture.is_some(),
        }
    }
}

/// Per-field texture-ref collection, implemented as a no-op for scalar field
/// types so the `ext_struct!` macro can invoke it on EVERY field — new texture
/// slots are enumerated without anyone remembering to update a list.
trait CollectTextureRefs {
    fn collect_texture_refs<'a>(&'a self, _out: &mut Vec<&'a TextureRef>) {}
}
impl CollectTextureRefs for f32 {}
impl CollectTextureRefs for [f32; 3] {}
impl CollectTextureRefs for Option<TextureRef> {
    fn collect_texture_refs<'a>(&'a self, out: &mut Vec<&'a TextureRef>) {
        if let Some(t) = self {
            out.push(t);
        }
    }
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
            texture_capabilities: None,
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    /// Sprite-sheet (atlas) animation — unlit, time-driven cell selection.
    /// Requires the `flipbook` feature on `awsm-renderer` at build time.
    /// The material's BASE-COLOR texture slot is the atlas; `base_color`
    /// is the tint. Cell selection runs on the renderer clock (no keyframes).
    FlipBook {
        /// Atlas grid columns (cells per row).
        cols: u32,
        /// Atlas grid rows.
        rows: u32,
        /// Cells actually used (≤ cols × rows; trailing grid cells unused).
        frame_count: u32,
        /// Playback rate, cells per second.
        fps: f32,
        /// Seconds added to the clock before cell selection (phase shift).
        #[serde(default)]
        time_offset: f32,
        /// How the running frame index wraps past the end.
        #[serde(default)]
        mode: FlipBookPlayMode,
        /// Row-indexing direction: `true` puts cell 0 on the BOTTOM row
        /// (texture-V-up authored atlases).
        #[serde(default)]
        flip_y: bool,
    },
}

/// FlipBook wrap mode — mirrors the renderer's `FlipBookMode` 1:1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum FlipBookPlayMode {
    /// `frame % count` — wraps forever.
    #[default]
    Loop,
    /// `0,1,…,N-1,N-2,…,1,0,…` (period `2N − 2`).
    PingPong,
    /// Sticks on the last cell.
    Clamp,
    /// Like `Clamp`, but past the end alpha = 0 (the quad disappears —
    /// pairs with Blend or Mask alpha modes).
    Once,
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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

impl PbrExtensions {
    /// Texture refs across every enabled extension (each extension's side is
    /// macro-generated, so new slots enumerate automatically; the per-extension
    /// list here mirrors [`Self::merged_over`]).
    fn texture_refs<'a>(&'a self, out: &mut Vec<&'a TextureRef>) {
        macro_rules! collect {
            ($($f:ident),* $(,)?) => {
                $(if let Some(e) = &self.$f { e.texture_refs(out); })*
            };
        }
        collect!(
            emissive_strength,
            ior,
            specular,
            transmission,
            diffuse_transmission,
            volume,
            clearcoat,
            sheen,
            dispersion,
            anisotropy,
            iridescence,
        );
    }

    /// The per-mesh MERGED view of a node's `inline` extension layer over the
    /// shared library `variant`: per extension, the inline value wins when
    /// present, otherwise the variant's authored values carry through.
    /// Extensions are STRICT capabilities: the ENABLE set comes from the
    /// shared `variant` alone (an extension is pipeline-shaped — enabling one
    /// is a material edit, never a per-node override), while an enabled
    /// extension's per-mesh PARAMETERS come from `inline` when seeded there.
    /// An inline-only extension (variant disabled) is dropped — it can't
    /// render without the variant's compiled code. THE single definition of
    /// this rule: the editor's mesh materialization and the inspector's
    /// extension controls both call this, so what the UI shows is what
    /// actually renders.
    pub fn merged_over(inline: &Self, variant: &Self) -> Self {
        Self {
            emissive_strength: variant
                .emissive_strength
                .map(|v| inline.emissive_strength.unwrap_or(v)),
            ior: variant.ior.map(|v| inline.ior.unwrap_or(v)),
            specular: variant.specular.map(|v| inline.specular.unwrap_or(v)),
            transmission: variant.transmission.map(|v| inline.transmission.unwrap_or(v)),
            diffuse_transmission: variant
                .diffuse_transmission
                .map(|v| inline.diffuse_transmission.unwrap_or(v)),
            volume: variant.volume.map(|v| inline.volume.unwrap_or(v)),
            clearcoat: variant.clearcoat.map(|v| inline.clearcoat.unwrap_or(v)),
            sheen: variant.sheen.map(|v| inline.sheen.unwrap_or(v)),
            dispersion: variant.dispersion.map(|v| inline.dispersion.unwrap_or(v)),
            anisotropy: variant.anisotropy.map(|v| inline.anisotropy.unwrap_or(v)),
            iridescence: variant.iridescence.map(|v| inline.iridescence.unwrap_or(v)),
        }
    }
}

macro_rules! ext_struct {
    ($(#[$m:meta])* $name:ident { $($field:ident : $ty:ty = $def:expr),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
        #[serde(rename_all = "snake_case")]
        pub struct $name { $(#[serde(default)] pub $field: $ty),* }
        impl Default for $name {
            fn default() -> Self { Self { $($field: $def),* } }
        }
        impl $name {
            /// Texture refs carried by this extension (generated per field —
            /// scalar fields no-op via [`CollectTextureRefs`]).
            fn texture_refs<'a>(&'a self, out: &mut Vec<&'a TextureRef>) {
                $(CollectTextureRefs::collect_texture_refs(&self.$field, out);)*
            }
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
/// GPU texture at load time via `awsm-renderer-meshgen::procedural_texture::*`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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

/// The SEMANTIC ROLE of a raster texture — what the renderer needs to know to
/// upload it correctly (color space + per-kind mipmap generation). This is the
/// source of truth set at import (from the glTF material slot, see
/// `renderer-gltf::populate`) and PERSISTED on the asset so a Save→reload
/// re-uploads with the same meaning instead of re-guessing. Mirrors the renderer's
/// `MipmapTextureKind`; the editor maps it back to a full `TextureColorInfo`
/// (sRGB-decode for color kinds, verbatim/linear for data kinds).
///
/// Without this, a reloaded normal/MR/occlusion map decodes as sRGB albedo →
/// corrupted normals/roughness + wrong-kind mipmaps (the save→reload shading drift).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TextureColorKind {
    /// Base color / albedo — sRGB. The safe default for any untagged texture.
    #[default]
    Albedo,
    /// Tangent-space normal map — LINEAR.
    Normal,
    /// Packed metallic-roughness — LINEAR.
    MetallicRoughness,
    /// Ambient occlusion — LINEAR.
    Occlusion,
    /// Emissive — sRGB.
    Emissive,
    /// Specular (factor) — LINEAR.
    Specular,
    /// Specular color — sRGB.
    SpecularColor,
    /// Transmission — LINEAR.
    Transmission,
    /// Volume thickness — LINEAR.
    VolumeThickness,
}

impl TextureColorKind {
    /// Whether this kind's image is sRGB-encoded (color) vs linear (data). Drives
    /// `TextureColorInfo::srgb_to_linear`. Kept here (next to the kinds) so the
    /// import and reload paths can't disagree.
    pub fn is_srgb(self) -> bool {
        matches!(self, Self::Albedo | Self::Emissive | Self::SpecularColor)
    }
}

/// Texture asset variants.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TextureDef {
    /// External raster file (PNG, JPEG, WebP) shipped alongside the
    /// project. `display_name` is the user-facing label + provides the
    /// extension for the on-disk file; the disk path itself is derived
    /// from the entry's `content_hash` (see
    /// `AssetEntry::content_hash`), not from this string.
    Raster {
        display_name: String,
        /// The texture's semantic role — its color space + mipmap kind. `None`
        /// for projects saved before this was tracked (the editor falls back to
        /// inferring from the import-assigned `display_name` slot suffix on load).
        #[serde(default)]
        color_kind: Option<TextureColorKind>,
    },
    /// Procedurally generated at load time.
    Procedural(ProceduralTextureDef),
}

/// File extension for a baked mesh blob side file — `<asset-id>.mesh.bin`.
/// The geometry ([`crate::mesh::MeshBlob`]) lives at `assets/<asset-id>.mesh.bin`
/// alongside `scene.toml`, keeping the document small.
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
