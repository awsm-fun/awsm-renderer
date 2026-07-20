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
    /// SSR participation mask (0..1, default 1). Multiplies the reflection
    /// descriptor's F0: 0 fully opts this material out of RECEIVING
    /// screen-space/BVH/probe reflections (IBL specular is kept — the
    /// SSR<->IBL crossfade reads the same masked value); fractional values
    /// damp them artistically. Decouples "how glossy it looks" (roughness)
    /// from "does SSR own its reflection". Per-mesh uniform, no recompile.
    #[serde(default = "default_one")]
    pub ssr_mask: f32,
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

    /// Visit every texture USE this material carries — each slot's ref paired
    /// with its semantic role ([`TextureColorKind`]), mirroring the player's
    /// per-slot bind semantics (scene-loader `resolve_material` /
    /// `bind_extension_textures`: `Normal` where the player binds normal-kind
    /// mips, `is_srgb()` matching its sRGB flag). The bundle bake resolves each
    /// use's KTX2 encoding from the kind and rewrites `TextureRef::asset` to
    /// per-encoding variant artifacts in the same walk
    /// (docs/plans/compression.md F2). Kept in lockstep with
    /// [`Self::texture_refs`] by the `texture_uses_match_texture_refs` test.
    pub fn for_each_texture_use_mut(
        &mut self,
        mut f: impl FnMut(TextureColorKind, &mut TextureRef),
    ) {
        use TextureColorKind as K;
        let mut visit = |kind: K, slot: &mut Option<TextureRef>| {
            if let Some(t) = slot {
                f(kind, t);
            }
        };
        visit(K::Albedo, &mut self.base_color_texture);
        visit(K::MetallicRoughness, &mut self.metallic_roughness_texture);
        visit(K::Emissive, &mut self.emissive_texture);
        visit(K::Normal, &mut self.normal_texture);
        visit(K::Occlusion, &mut self.occlusion_texture);
        let ext = &mut self.extensions;
        if let Some(e) = &mut ext.specular {
            visit(K::Specular, &mut e.tex);
            visit(K::SpecularColor, &mut e.color_tex);
        }
        if let Some(e) = &mut ext.transmission {
            visit(K::Transmission, &mut e.tex);
        }
        if let Some(e) = &mut ext.diffuse_transmission {
            visit(K::Transmission, &mut e.tex);
            visit(K::SpecularColor, &mut e.color_tex);
        }
        if let Some(e) = &mut ext.volume {
            visit(K::VolumeThickness, &mut e.thickness_tex);
        }
        if let Some(e) = &mut ext.clearcoat {
            visit(K::Specular, &mut e.tex);
            visit(K::MetallicRoughness, &mut e.roughness_tex);
            visit(K::Normal, &mut e.normal_tex);
        }
        if let Some(e) = &mut ext.sheen {
            visit(K::SpecularColor, &mut e.color_tex);
            visit(K::MetallicRoughness, &mut e.roughness_tex);
        }
        if let Some(e) = &mut ext.anisotropy {
            // Anisotropy direction maps are linear DATA, not tangent normals:
            // they must never ride the two-channel normal packing (their
            // shader sampling isn't Z-reconstruct-aware), so they resolve as
            // MetallicRoughness-kind (ETC1S under Auto — the pre-F2 shipped
            // behavior) even though the player binds them with normal-kind
            // mips.
            visit(K::MetallicRoughness, &mut e.tex);
        }
        if let Some(e) = &mut ext.iridescence {
            visit(K::Specular, &mut e.tex);
            visit(K::VolumeThickness, &mut e.thickness_tex);
        }
        if let Some(e) = &mut ext.secondary_maps {
            visit(K::Albedo, &mut e.base_color_tex);
            visit(K::Normal, &mut e.normal_tex);
            visit(K::MetallicRoughness, &mut e.metallic_roughness_tex);
            visit(K::Occlusion, &mut e.occlusion_tex);
            visit(K::Emissive, &mut e.emissive_tex);
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
            ssr_mask: 1.0,
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
    /// Detail / secondary maps (engine extension, not a KHR one): a second,
    /// typically high-tiled texture per core PBR slot, blended over the
    /// primary before shading. Not part of glTF — bundles carry it in the
    /// scene's own material table.
    pub secondary_maps: Option<SecondaryMapsExt>,
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
            secondary_maps,
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
            transmission: variant
                .transmission
                .map(|v| inline.transmission.unwrap_or(v)),
            diffuse_transmission: variant
                .diffuse_transmission
                .map(|v| inline.diffuse_transmission.unwrap_or(v)),
            volume: variant.volume.map(|v| inline.volume.unwrap_or(v)),
            clearcoat: variant.clearcoat.map(|v| inline.clearcoat.unwrap_or(v)),
            sheen: variant.sheen.map(|v| inline.sheen.unwrap_or(v)),
            dispersion: variant.dispersion.map(|v| inline.dispersion.unwrap_or(v)),
            anisotropy: variant.anisotropy.map(|v| inline.anisotropy.unwrap_or(v)),
            iridescence: variant.iridescence.map(|v| inline.iridescence.unwrap_or(v)),
            secondary_maps: variant
                .secondary_maps
                .map(|v| inline.secondary_maps.unwrap_or(v)),
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
ext_struct!(/// Detail / secondary maps (engine extension). One optional secondary
/// texture per core PBR slot, blended over the primary AFTER its factor is
/// applied: base color = ×2 multiply (mid-grey neutral), normal = RNM
/// detail blend, metallic-roughness = roughness overlay + metallic
/// multiply, occlusion = multiply (cavity), emissive = additive. Each
/// slot's `TextureRef` carries its OWN uv transform / sampler / flow —
/// tile detail sets via `transform.scale` (matched scales recommended for
/// physically coherent sets). Each `*_strength` (0..1, per-mesh uniform)
/// lerps that slot's sample toward its blend-neutral value, so strength 0
/// is exactly "slot off". Unset slots are skipped in-shader.
SecondaryMapsExt {
    base_color_tex: Option<TextureRef> = None,
    normal_tex: Option<TextureRef> = None,
    metallic_roughness_tex: Option<TextureRef> = None,
    occlusion_tex: Option<TextureRef> = None,
    emissive_tex: Option<TextureRef> = None,
    base_color_strength: f32 = 1.0,
    normal_strength: f32 = 1.0,
    metallic_roughness_strength: f32 = 1.0,
    occlusion_strength: f32 = 1.0,
    emissive_strength: f32 = 1.0,
});

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

#[cfg(test)]
mod texture_use_tests {
    use super::*;
    use crate::primitive::TextureRef;
    use crate::AssetId;

    /// Every slot `texture_refs()` enumerates must also be visited (with a
    /// kind) by `for_each_texture_use_mut` — the drift guard for new
    /// `ext_struct!` texture slots, which auto-enumerate in `texture_refs()`
    /// but need a manual kind mapping in the use walk.
    #[test]
    fn texture_uses_match_texture_refs() {
        let t = || Some(TextureRef::new(AssetId::new()));
        let mut def = MaterialDef {
            base_color_texture: t(),
            metallic_roughness_texture: t(),
            emissive_texture: t(),
            normal_texture: t(),
            occlusion_texture: t(),
            extensions: PbrExtensions {
                emissive_strength: Some(Default::default()),
                ior: Some(Default::default()),
                specular: Some(SpecularExt {
                    tex: t(),
                    color_tex: t(),
                    ..Default::default()
                }),
                transmission: Some(TransmissionExt {
                    tex: t(),
                    ..Default::default()
                }),
                diffuse_transmission: Some(DiffuseTransmissionExt {
                    tex: t(),
                    color_tex: t(),
                    ..Default::default()
                }),
                volume: Some(VolumeExt {
                    thickness_tex: t(),
                    ..Default::default()
                }),
                clearcoat: Some(ClearcoatExt {
                    tex: t(),
                    roughness_tex: t(),
                    normal_tex: t(),
                    ..Default::default()
                }),
                sheen: Some(SheenExt {
                    color_tex: t(),
                    roughness_tex: t(),
                    ..Default::default()
                }),
                dispersion: Some(Default::default()),
                anisotropy: Some(AnisotropyExt {
                    tex: t(),
                    ..Default::default()
                }),
                iridescence: Some(IridescenceExt {
                    tex: t(),
                    thickness_tex: t(),
                    ..Default::default()
                }),
                secondary_maps: Some(SecondaryMapsExt {
                    base_color_tex: t(),
                    normal_tex: t(),
                    metallic_roughness_tex: t(),
                    occlusion_tex: t(),
                    emissive_tex: t(),
                    ..Default::default()
                }),
            },
            ..Default::default()
        };
        let ref_assets: Vec<AssetId> = def.texture_refs().iter().map(|t| t.asset).collect();
        let mut use_assets = Vec::new();
        def.for_each_texture_use_mut(|_, t| use_assets.push(t.asset));
        let sorted = |mut v: Vec<AssetId>| {
            v.sort_by_key(|id| id.0);
            v
        };
        assert_eq!(
            sorted(ref_assets),
            sorted(use_assets),
            "for_each_texture_use_mut must visit exactly the slots texture_refs() enumerates \
             — add the kind mapping for the new ext_struct! texture slot"
        );
    }

    /// The normal slot resolves as Normal-kind; base color as sRGB Albedo.
    #[test]
    fn slot_kinds_are_stable() {
        let mut def = MaterialDef {
            base_color_texture: Some(TextureRef::new(AssetId::new())),
            normal_texture: Some(TextureRef::new(AssetId::new())),
            ..Default::default()
        };
        let mut kinds = Vec::new();
        def.for_each_texture_use_mut(|k, _| kinds.push(k));
        assert_eq!(
            kinds,
            vec![TextureColorKind::Albedo, TextureColorKind::Normal]
        );
        assert!(TextureColorKind::Albedo.is_srgb());
        assert!(!TextureColorKind::Normal.is_srgb());
    }
}
