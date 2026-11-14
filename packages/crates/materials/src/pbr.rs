//! Physically based rendering (PBR) material parameters and packing.

use crate::{
    shader::MaterialShader,
    writer::{write, write_material_texture, write_material_texture_or_skip},
    MaterialAlphaMode, MaterialShaderId, MaterialTexture, TextureContext,
};

/// WGSL helper module for this material.
///
/// PBR ships two helper files (the storage-buffer accessor + the material-
/// color path). The renderer treats the concatenation as a single fragment.
pub const WGSL_FRAGMENT: &str = concat!(
    include_str!("wgsl/pbr/pbr_material.wgsl"),
    "\n",
    include_str!("wgsl/pbr/pbr_material_color.wgsl"),
);

/// Physically based rendering (PBR) material parameters.
#[derive(Clone, Debug)]
pub struct PbrMaterial {
    pub base_color_tex: Option<MaterialTexture>,
    pub base_color_factor: [f32; 4],

    pub metallic_roughness_tex: Option<MaterialTexture>,
    pub metallic_factor: f32,
    pub roughness_factor: f32,

    pub normal_tex: Option<MaterialTexture>,
    pub normal_scale: f32,

    pub occlusion_tex: Option<MaterialTexture>,
    pub occlusion_strength: f32,

    pub emissive_tex: Option<MaterialTexture>,
    pub emissive_factor: [f32; 3],

    /// Debug settings.
    pub debug: PbrMaterialDebug,

    /// SSR participation mask (0..1, default 1). Multiplies the reflection
    /// descriptor's F0 before it is written: 0 fully opts this material out
    /// of RECEIVING screen-space/BVH/probe reflections (IBL specular is
    /// kept automatically — the SSR<->IBL crossfade reads the same masked
    /// value), fractional values damp them artistically. Decouples "how
    /// glossy it looks" (roughness) from "does SSR own its reflection".
    pub ssr_mask: f32,
    /// Two-channel-normal shader packing (docs/plans/compression.md F3),
    /// two bit-pairs: bits 0-1 = the main normal map, bits 2-3 = the
    /// clearcoat normal map. Per pair: 0 = regular full-RGB normal, 1 = X/Y
    /// in `.rg` (BC5 / EAC-RG11 transcode), 2 = packed RGBA layout (X in
    /// `.rgb`, Y in `.a`). The shader reconstructs Z = sqrt(1 - x² - y²)
    /// for non-zero pairs. Runtime uniform — never splits pipelines.
    pub normal_packing: u32,

    // Non-core features and extensions
    pub vertex_color_info: Option<PbrMaterialVertexColorInfo>,
    pub emissive_strength: Option<PbrMaterialEmissiveStrength>,
    pub ior: Option<PbrMaterialIor>,
    pub specular: Option<PbrMaterialSpecular>,
    pub transmission: Option<PbrMaterialTransmission>,
    pub diffuse_transmission: Option<PbrMaterialDiffuseTransmission>,
    pub volume: Option<PbrMaterialVolume>,
    pub clearcoat: Option<PbrMaterialClearCoat>,
    pub sheen: Option<PbrMaterialSheen>,
    pub dispersion: Option<PbrMaterialDispersion>,
    pub anisotropy: Option<PbrMaterialAnisotropy>,
    pub iridescence: Option<PbrMaterialIridescence>,
    pub secondary_maps: Option<PbrMaterialSecondaryMaps>,

    // Things that affect shader generation and therefore can't be changed
    // dynamically — create a new material instead.
    alpha_mode: MaterialAlphaMode,
    double_sided: bool,
}

/// Compile-time feature set of a PBR material: which optional code paths
/// its specialized shader actually needs. The five core texture-slot bits
/// are RETIRED (permanently false): slot sampling is always compiled and an
/// unbound slot packs the shared 1×1 NEUTRAL, so texture binds are pure
/// data and never key a pipeline. The extension bits are STRICT (derived
/// from the `Option` fields: an enabled extension's code runs
/// unconditionally for every instance). This is the input to:
///
/// 1. **Compile-time gating** — the Askama `{% if pbr_features.<x> %}`
///    checks in the PBR shader (opaque compute + transparent fragment) for
///    the EXTENSION features, so a material without (say) clearcoat compiles
///    no clearcoat code. (Texture-slot sampling is NOT gated — it is always
///    present and unbound slots sample the 1×1 neutral.)
/// 2. **Bucket identity** — [`Self::bits`] is the stable feature-hash that
///    maps a feature-set to its own `shader_id` / bucket, so two materials
///    with the same feature-set share one specialized pipeline. For the
///    OPAQUE kernels the set that is hashed is not the material's own but
///    [`PbrSpecialization::kernel_features`] of it: by default the features
///    that are free to carry unused are compiled into every kernel, so only
///    the rest split buckets.
///
/// `double_sided` and `alpha_mode` are deliberately NOT here — the former
/// is raster state (a pipeline-variant dimension, not shader code), and
/// the latter routes opaque → visibility-buffer compute bucket vs.
/// transparent → forward per-mesh pipeline (both specialized per
/// feature-set), so it's a routing decision, not a feature bit.
///
/// Bit layout (see [`Self::bits`]) is stable and append-only: never
/// renumber an existing bit (it would silently remap every persisted /
/// cached feature-hash).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PbrFeatures {
    pub base_color_tex: bool,
    pub metallic_roughness_tex: bool,
    pub normal_tex: bool,
    pub occlusion_tex: bool,
    pub emissive_tex: bool,
    pub vertex_color: bool,
    pub emissive_strength: bool,
    pub ior: bool,
    pub specular: bool,
    pub transmission: bool,
    pub diffuse_transmission: bool,
    pub volume: bool,
    pub clearcoat: bool,
    pub sheen: bool,
    pub dispersion: bool,
    pub anisotropy: bool,
    pub iridescence: bool,
    pub secondary_maps: bool,
}

impl PbrFeatures {
    /// Bit positions in [`Self::bits`] — stable + append-only.
    const BIT_BASE_COLOR_TEX: u32 = 1 << 0;
    const BIT_METALLIC_ROUGHNESS_TEX: u32 = 1 << 1;
    const BIT_NORMAL_TEX: u32 = 1 << 2;
    const BIT_OCCLUSION_TEX: u32 = 1 << 3;
    const BIT_EMISSIVE_TEX: u32 = 1 << 4;
    const BIT_VERTEX_COLOR: u32 = 1 << 5;
    const BIT_EMISSIVE_STRENGTH: u32 = 1 << 6;
    const BIT_IOR: u32 = 1 << 7;
    const BIT_SPECULAR: u32 = 1 << 8;
    const BIT_TRANSMISSION: u32 = 1 << 9;
    const BIT_DIFFUSE_TRANSMISSION: u32 = 1 << 10;
    const BIT_VOLUME: u32 = 1 << 11;
    const BIT_CLEARCOAT: u32 = 1 << 12;
    const BIT_SHEEN: u32 = 1 << 13;
    const BIT_DISPERSION: u32 = 1 << 14;
    const BIT_ANISOTROPY: u32 = 1 << 15;
    const BIT_IRIDESCENCE: u32 = 1 << 16;
    const BIT_SECONDARY_MAPS: u32 = 1 << 17;

    /// The number of distinct feature bits (≤ 32 so [`Self::bits`] fits a u32).
    pub const COUNT: u32 = 18;

    /// Derives the feature set from a material. The five texture-slot bits
    /// are RETIRED — always `false` (bit positions kept for the stable
    /// append-only layout): slot sampling code is unconditionally present
    /// and unbound slots sample the 1×1 NEUTRAL, so texture presence never
    /// splits buckets. Extension bits remain presence-derived (strict).
    pub fn from_material(m: &PbrMaterial) -> Self {
        Self {
            base_color_tex: false,
            metallic_roughness_tex: false,
            normal_tex: false,
            occlusion_tex: false,
            emissive_tex: false,
            vertex_color: m.vertex_color_info.is_some(),
            emissive_strength: m.emissive_strength.is_some(),
            ior: m.ior.is_some(),
            specular: m.specular.is_some(),
            transmission: m.transmission.is_some(),
            diffuse_transmission: m.diffuse_transmission.is_some(),
            volume: m.volume.is_some(),
            clearcoat: m.clearcoat.is_some(),
            sheen: m.sheen.is_some(),
            dispersion: m.dispersion.is_some(),
            anisotropy: m.anisotropy.is_some(),
            iridescence: m.iridescence.is_some(),
            secondary_maps: m.secondary_maps.is_some(),
        }
    }

    /// Every feature on. Behaviourally identical to a non-specialized
    /// (always-all-extensions) PBR shader. NOT used by the render pipeline
    /// — every bucket compiles a real per-feature-set mask, never this — it
    /// exists only as a test fixture for the feature-gating tests.
    pub fn all() -> Self {
        Self {
            base_color_tex: true,
            metallic_roughness_tex: true,
            normal_tex: true,
            occlusion_tex: true,
            emissive_tex: true,
            vertex_color: true,
            emissive_strength: true,
            ior: true,
            specular: true,
            transmission: true,
            diffuse_transmission: true,
            volume: true,
            clearcoat: true,
            sheen: true,
            dispersion: true,
            anisotropy: true,
            iridescence: true,
            secondary_maps: true,
        }
    }

    /// The features that cost nothing to carry unused: vertex colour,
    /// emissive strength and IOR. Together they are about 450 bytes of WGSL
    /// in a ~240 KB opaque kernel, and a material without one of them pays
    /// an index test and a neutral constant for it. Measured on an Apple M1
    /// (GPU timestamp queries, 1080p and 4K): compiled into a kernel that
    /// shades materials without them, they move the opaque pass by at most
    /// 4 % and the frame by at most 0.5 %. They are what
    /// [`PbrSpecialization::ExpensiveFeatures`] compiles into every opaque
    /// PBR kernel.
    ///
    /// A feature belongs here only if a kernel compiled WITH it
    ///
    /// 1. shades a material WITHOUT it exactly as a kernel compiled without
    ///    it would (the extension loaders give that: `pbr_material_load_*`
    ///    return the glTF-absent value at index 0, and the vertex-colour
    ///    multiply is skipped when the material has none), and
    /// 2. costs no measurable GPU time doing so.
    ///
    /// Specular, transmission and volume pass the first test and fail the
    /// second (each about 2.5 % of frame time on that M1; see
    /// [`Self::neutral`]), so they keep a kernel of their own. Anisotropy
    /// fails the first (its lobe replaces the isotropic one), and the
    /// remaining lobes are real per-pixel work.
    pub fn cheap() -> Self {
        Self {
            vertex_color: true,
            emissive_strength: true,
            ior: true,
            ..Self::default()
        }
    }

    /// Every feature whose unused code leaves a material's shading unchanged:
    /// [`Self::cheap`] plus specular, transmission, volume and dispersion —
    /// 540 bytes of WGSL between them. [`PbrSpecialization::LobesOnly`]
    /// compiles all seven into every opaque PBR kernel.
    ///
    /// The four that are not in [`Self::cheap`] are neutral but not free.
    /// Carried by a kernel whose materials lack them, on an Apple M1 at
    /// 1080p: specular +14 % of the opaque pass (two more texture-slot
    /// lookups per pixel, and an F0 / f90 the compiler can no longer fold
    /// per light), transmission +12 %, volume +12 %, dispersion nothing on
    /// its own; all seven together +64 %, because the transmission path
    /// (refraction, up to three environment fetches) becomes live code.
    pub fn neutral() -> Self {
        Self {
            specular: true,
            transmission: true,
            volume: true,
            dispersion: true,
            ..Self::cheap()
        }
    }

    /// Every feature that is in `self` or in `other`.
    pub fn union(self, other: Self) -> Self {
        Self::from_bits(self.bits() | other.bits())
    }

    /// Stable one-bit-per-feature packing. Doubles as the feature-hash
    /// keying a feature-set to its opaque bucket.
    pub fn bits(&self) -> u32 {
        let mut b = 0u32;
        if self.base_color_tex {
            b |= Self::BIT_BASE_COLOR_TEX;
        }
        if self.metallic_roughness_tex {
            b |= Self::BIT_METALLIC_ROUGHNESS_TEX;
        }
        if self.normal_tex {
            b |= Self::BIT_NORMAL_TEX;
        }
        if self.occlusion_tex {
            b |= Self::BIT_OCCLUSION_TEX;
        }
        if self.emissive_tex {
            b |= Self::BIT_EMISSIVE_TEX;
        }
        if self.vertex_color {
            b |= Self::BIT_VERTEX_COLOR;
        }
        if self.emissive_strength {
            b |= Self::BIT_EMISSIVE_STRENGTH;
        }
        if self.ior {
            b |= Self::BIT_IOR;
        }
        if self.specular {
            b |= Self::BIT_SPECULAR;
        }
        if self.transmission {
            b |= Self::BIT_TRANSMISSION;
        }
        if self.diffuse_transmission {
            b |= Self::BIT_DIFFUSE_TRANSMISSION;
        }
        if self.volume {
            b |= Self::BIT_VOLUME;
        }
        if self.clearcoat {
            b |= Self::BIT_CLEARCOAT;
        }
        if self.sheen {
            b |= Self::BIT_SHEEN;
        }
        if self.dispersion {
            b |= Self::BIT_DISPERSION;
        }
        if self.anisotropy {
            b |= Self::BIT_ANISOTROPY;
        }
        if self.iridescence {
            b |= Self::BIT_IRIDESCENCE;
        }
        if self.secondary_maps {
            b |= Self::BIT_SECONDARY_MAPS;
        }
        b
    }

    /// Inverse of [`Self::bits`].
    pub fn from_bits(b: u32) -> Self {
        Self {
            base_color_tex: b & Self::BIT_BASE_COLOR_TEX != 0,
            metallic_roughness_tex: b & Self::BIT_METALLIC_ROUGHNESS_TEX != 0,
            normal_tex: b & Self::BIT_NORMAL_TEX != 0,
            occlusion_tex: b & Self::BIT_OCCLUSION_TEX != 0,
            emissive_tex: b & Self::BIT_EMISSIVE_TEX != 0,
            vertex_color: b & Self::BIT_VERTEX_COLOR != 0,
            emissive_strength: b & Self::BIT_EMISSIVE_STRENGTH != 0,
            ior: b & Self::BIT_IOR != 0,
            specular: b & Self::BIT_SPECULAR != 0,
            transmission: b & Self::BIT_TRANSMISSION != 0,
            diffuse_transmission: b & Self::BIT_DIFFUSE_TRANSMISSION != 0,
            volume: b & Self::BIT_VOLUME != 0,
            clearcoat: b & Self::BIT_CLEARCOAT != 0,
            sheen: b & Self::BIT_SHEEN != 0,
            dispersion: b & Self::BIT_DISPERSION != 0,
            anisotropy: b & Self::BIT_ANISOTROPY != 0,
            iridescence: b & Self::BIT_IRIDESCENCE != 0,
            secondary_maps: b & Self::BIT_SECONDARY_MAPS != 0,
        }
    }
}

/// Which PBR features give a material an opaque kernel (bucket) of its own.
///
/// Every distinct feature set a scene's opaque PBR materials resolve to is
/// one more ~240 KB kernel to compile on a cold load, at roughly two seconds
/// of compile per megabyte of WGSL. The features themselves are a small part
/// of that text, so splitting on all of them buys little and costs a kernel
/// per combination. But a feature compiled into a kernel whose materials do
/// not use it is not always free to RUN. This policy decides, per feature,
/// between a kernel of its own and a place in every kernel.
///
/// Set once, on the builder
/// (`AwsmRendererBuilder::with_pbr_specialization`). It only affects the
/// opaque (visibility-buffer) kernels; the transparent pass keeps one
/// pipeline per exact feature set.
///
/// All three render a scene identically (checked bit for bit in the HDR
/// targets on an Apple M1 / Metal). They differ in how many kernels a load
/// compiles and in what a frame costs:
///
/// | | kernels | GPU time against `EveryFeature` (M1, 1080p–4K) |
/// |---|---|---|
/// | `ExpensiveFeatures` | one per combination of the ten features that are not free | opaque pass within ±5 %, under 1 % of the frame |
/// | `LobesOnly` | one per combination of the six lobes | opaque pass +15 to +68 %, frame +3 to +13 % |
/// | `EveryFeature` | one per exact feature set | — |
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PbrSpecialization {
    /// The default. The features that are free to carry unused
    /// ([`PbrFeatures::cheap`]: vertex colour, emissive strength, IOR) are
    /// compiled into EVERY opaque PBR kernel. Everything else — specular,
    /// transmission, volume, dispersion, clearcoat, sheen, iridescence,
    /// anisotropy, diffuse transmission, secondary maps — still splits
    /// buckets. Two materials that differ only in the first three share one
    /// kernel, at no cost per frame.
    #[default]
    ExpensiveFeatures,
    /// Fewer kernels, slower frames. Every feature whose unused code leaves
    /// shading unchanged ([`PbrFeatures::neutral`]: the cheap three, specular,
    /// transmission, volume, dispersion) is compiled into every opaque PBR
    /// kernel, and only the six lobes split buckets. A scene compiles one
    /// kernel fewer for each pair of materials that differ only in those,
    /// and every opaque PBR pixel then runs the unused code: on an Apple M1
    /// the opaque pass took 15–68 % longer and the frame 3–13 %. For an app
    /// whose cold-load time matters more than its frame time.
    LobesOnly,
    /// One kernel per exact feature set, with nothing compiled in that the
    /// material does not use — the behaviour up to 0.40. Smallest possible
    /// kernels, most of them.
    EveryFeature,
}

impl PbrSpecialization {
    /// The features compiled into every opaque PBR kernel under this policy,
    /// whether the material has them or not.
    pub fn always_compiled(self) -> PbrFeatures {
        match self {
            Self::ExpensiveFeatures => PbrFeatures::cheap(),
            Self::LobesOnly => PbrFeatures::neutral(),
            Self::EveryFeature => PbrFeatures::default(),
        }
    }

    /// The feature set of the opaque kernel that shades a material with
    /// `material` features: the material's own, plus whatever this policy
    /// compiles into every kernel. Always a superset of `material`, so the
    /// kernel never lacks code the material needs.
    pub fn kernel_features(self, material: PbrFeatures) -> PbrFeatures {
        material.union(self.always_compiled())
    }
}

/// Debug visualization modes for PBR materials.
#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash)]
pub enum PbrMaterialDebug {
    None,
    BaseColor,
    MetallicRoughness,
    Normals,
    Occlusion,
    Emissive,
    Specular,
}

impl PbrMaterialDebug {
    /// Returns the debug bitmask value.
    pub fn bitmask(&self) -> u32 {
        match self {
            PbrMaterialDebug::None => 0,
            PbrMaterialDebug::BaseColor => 1 << 0,
            PbrMaterialDebug::MetallicRoughness => 1 << 1,
            PbrMaterialDebug::Normals => 1 << 2,
            PbrMaterialDebug::Occlusion => 1 << 3,
            PbrMaterialDebug::Emissive => 1 << 4,
            PbrMaterialDebug::Specular => 1 << 5,
        }
    }
}

/// Vertex color metadata for PBR materials.
#[derive(Clone, Debug)]
pub struct PbrMaterialVertexColorInfo {
    pub set_index: u32,
}

/// Emissive strength extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialEmissiveStrength {
    pub strength: f32,
}

/// Index of refraction extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialIor {
    pub ior: f32,
}

/// Specular extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialSpecular {
    pub tex: Option<MaterialTexture>,
    pub factor: f32,
    pub color_tex: Option<MaterialTexture>,
    pub color_factor: [f32; 3],
}

/// Transmission extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialTransmission {
    pub tex: Option<MaterialTexture>,
    pub factor: f32,
}

/// Diffuse transmission extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialDiffuseTransmission {
    pub tex: Option<MaterialTexture>,
    pub factor: f32,
    pub color_tex: Option<MaterialTexture>,
    pub color_factor: [f32; 3],
}

/// Volume extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialVolume {
    pub thickness_tex: Option<MaterialTexture>,
    pub thickness_factor: f32,
    pub attenuation_distance: f32,
    pub attenuation_color: [f32; 3],
}

/// Clear coat extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialClearCoat {
    pub tex: Option<MaterialTexture>,
    pub factor: f32,
    pub roughness_tex: Option<MaterialTexture>,
    pub roughness_factor: f32,
    pub normal_tex: Option<MaterialTexture>,
    pub normal_scale: f32,
}

/// Sheen extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialSheen {
    pub roughness_tex: Option<MaterialTexture>,
    pub roughness_factor: f32,
    pub color_tex: Option<MaterialTexture>,
    pub color_factor: [f32; 3],
}

/// Dispersion extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialDispersion {
    pub dispersion: f32,
}

/// Anisotropy extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialAnisotropy {
    pub tex: Option<MaterialTexture>,
    pub strength: f32,
    pub rotation: f32,
}

/// Iridescence extension data.
#[derive(Clone, Debug)]
pub struct PbrMaterialIridescence {
    pub tex: Option<MaterialTexture>,
    pub factor: f32,
    pub ior: f32,
    pub thickness_tex: Option<MaterialTexture>,
    pub thickness_min: f32,
    pub thickness_max: f32,
}

/// Secondary / detail maps extension data (engine extension, not KHR).
/// One optional high-tiled texture per core PBR slot, blended over the
/// primary in the OPAQUE kernels (the transparent forward path ignores
/// them — implicit-derivative sampling there can't take the per-slot
/// skip branch). Unset slots pack the zero SKIP sentinel (`exists == 0`),
/// so the shader takes the per-slot branch and does no fetch at all;
/// each strength (0..1) lerps the sampled value toward that slot's
/// blend-neutral, making strength 0 exactly equal to "slot off".
#[derive(Clone, Debug, Default)]
pub struct PbrMaterialSecondaryMaps {
    pub base_color_tex: Option<MaterialTexture>,
    pub base_color_strength: f32,
    pub normal_tex: Option<MaterialTexture>,
    pub normal_strength: f32,
    pub metallic_roughness_tex: Option<MaterialTexture>,
    pub metallic_roughness_strength: f32,
    pub occlusion_tex: Option<MaterialTexture>,
    pub occlusion_strength: f32,
    pub emissive_tex: Option<MaterialTexture>,
    pub emissive_strength: f32,
}

impl PbrMaterial {
    /// Creates a PBR material with default parameters.
    pub fn new(alpha_mode: MaterialAlphaMode, double_sided: bool) -> Self {
        Self {
            base_color_tex: None,
            base_color_factor: [1.0, 1.0, 1.0, 1.0],
            metallic_roughness_tex: None,
            metallic_factor: 1.0,
            roughness_factor: 1.0,
            normal_tex: None,
            normal_scale: 1.0,
            occlusion_tex: None,
            occlusion_strength: 1.0,
            emissive_tex: None,
            emissive_factor: [0.0, 0.0, 0.0],
            vertex_color_info: None,
            emissive_strength: None,
            ior: None,
            specular: None,
            transmission: None,
            diffuse_transmission: None,
            volume: None,
            clearcoat: None,
            sheen: None,
            dispersion: None,
            anisotropy: None,
            iridescence: None,
            secondary_maps: None,
            debug: PbrMaterialDebug::None,
            ssr_mask: 1.0,
            normal_packing: 0,
            alpha_mode,
            double_sided,
        }
    }

    /// Returns true if the material has any transmission effect
    /// (either via transmission_factor > 0 or a transmission texture)
    pub fn has_transmission(&self) -> bool {
        match &self.transmission {
            Some(transmission) => transmission.factor > 0.0 || transmission.tex.is_some(),
            None => false,
        }
    }

    /// Returns the material alpha mode by reference.
    pub fn alpha_mode(&self) -> &MaterialAlphaMode {
        &self.alpha_mode
    }

    /// Returns whether the material is double sided.
    pub fn double_sided(&self) -> bool {
        self.double_sided
    }

    /// Returns the alpha cutoff for masked materials.
    pub fn alpha_cutoff(&self) -> Option<f32> {
        match self.alpha_mode {
            MaterialAlphaMode::Mask { cutoff } => Some(cutoff),
            _ => None,
        }
    }

    /// Set the alpha-test cutoff — a no-op unless the material is already in `Mask`
    /// alpha mode (the mode itself is a pipeline choice, not a uniform tweak). Used
    /// by the settable/animatable `AlphaCutoff` built-in param.
    pub fn set_alpha_cutoff(&mut self, cutoff: f32) {
        if let MaterialAlphaMode::Mask { cutoff: c } = &mut self.alpha_mode {
            *c = cutoff;
        }
    }

    /// Returns true if alpha blending is enabled.
    pub fn has_alpha_blend(&self) -> bool {
        matches!(self.alpha_mode, MaterialAlphaMode::Blend)
    }
}

/// Shared modules the PBR shading chain uses (closure pulls MATH/CAMERA). The
/// full set — PBR is the heavy path the skinny work exists to keep *off* simpler
/// materials. See [`MaterialShader::shader_includes`].
pub const SHADER_INCLUDES: crate::ShaderIncludes = crate::ShaderIncludes::MATERIAL_COLOR_CALC
    .union(crate::ShaderIncludes::APPLY_LIGHTING)
    .union(crate::ShaderIncludes::LIGHT_ACCESS)
    .union(crate::ShaderIncludes::BRDF)
    .union(crate::ShaderIncludes::TEXTURES)
    .union(crate::ShaderIncludes::CAMERA)
    .union(crate::ShaderIncludes::COLOR_SPACE)
    .union(crate::ShaderIncludes::SHADOWS)
    .union(crate::ShaderIncludes::EXTRAS)
    .union(crate::ShaderIncludes::VERTEX_COLOR);

/// Pre-shade inputs the PBR body consumes.
pub const FRAGMENT_INPUTS: crate::FragmentInputs = crate::FragmentInputs::NORMALS
    .union(crate::FragmentInputs::TANGENTS)
    .union(crate::FragmentInputs::UV)
    .union(crate::FragmentInputs::LIGHTS)
    .union(crate::FragmentInputs::VIEW_DIR)
    .union(crate::FragmentInputs::VERTEX_COLOR);

impl MaterialShader for PbrMaterial {
    fn shader_id(&self) -> MaterialShaderId {
        MaterialShaderId::PBR
    }

    fn wgsl_fragment(&self) -> &'static str {
        WGSL_FRAGMENT
    }

    fn shader_includes(&self) -> crate::ShaderIncludes {
        SHADER_INCLUDES
    }

    fn fragment_inputs(&self) -> crate::FragmentInputs {
        FRAGMENT_INPUTS
    }

    fn alpha_mode(&self) -> MaterialAlphaMode {
        self.alpha_mode
    }

    fn is_transparency_pass(&self) -> bool {
        // MASK is NOT transparency: per glTF it's alpha-tested OPAQUE, so it
        // belongs in the visibility/opaque path (where it lands in `opaque_tex`
        // for transmission to sample, casts shadows, and is deferred-shaded). The
        // per-fragment cutoff is applied by the masked variant of the `geometry`
        // raster pass (`alpha_test` in `ShaderCacheKeyGeometry`), which discards
        // fragments whose base-color alpha < `alpha_cutoff` — NOT here. Only BLEND
        // (order-dependent) and TRANSMISSION (samples the opaque framebuffer)
        // route to the forward transparent pass.
        self.has_alpha_blend() || self.has_transmission()
    }

    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, data: &mut Vec<u8>) {
        write(data, self.shader_id().as_u32().into());

        write(data, self.alpha_mode.variant_as_u32().into());
        write(data, self.alpha_cutoff().unwrap_or(0.0f32).into());

        write_material_texture(
            data,
            self.base_color_tex.as_ref(),
            ctx,
            crate::NeutralTexture::White,
        );
        write(data, self.base_color_factor[0].into());
        write(data, self.base_color_factor[1].into());
        write(data, self.base_color_factor[2].into());
        write(data, self.base_color_factor[3].into());

        write_material_texture(
            data,
            self.metallic_roughness_tex.as_ref(),
            ctx,
            crate::NeutralTexture::White,
        );
        write(data, self.metallic_factor.into());
        write(data, self.roughness_factor.into());

        write_material_texture(
            data,
            self.normal_tex.as_ref(),
            ctx,
            crate::NeutralTexture::FlatNormal,
        );
        write(data, self.normal_scale.into());

        write_material_texture(
            data,
            self.occlusion_tex.as_ref(),
            ctx,
            crate::NeutralTexture::White,
        );
        write(data, self.occlusion_strength.into());

        write_material_texture(
            data,
            self.emissive_tex.as_ref(),
            ctx,
            crate::NeutralTexture::White,
        );
        write(data, self.emissive_factor[0].into());
        write(data, self.emissive_factor[1].into());
        write(data, self.emissive_factor[2].into());

        write(data, self.debug.bitmask().into());
        // ssr_mask rides at the END of the core header (word 39) so no
        // existing offset shifts — see PBR_CORE_WORDS in pbr_material.wgsl.
        write(data, self.ssr_mask.into());
        // normal_packing appends after it (word 40) for the same reason.
        write(data, self.normal_packing.into());

        // Feature indices.
        #[derive(Default, Debug)]
        struct FeatureIndices {
            pub vertex_color_info: u32,
            pub emissive_strength: u32,
            pub ior: u32,
            pub specular: u32,
            pub transmission: u32,
            pub diffuse_transmission: u32,
            pub volume: u32,
            pub clearcoat: u32,
            pub sheen: u32,
            pub dispersion: u32,
            pub anisotropy: u32,
            pub iridescence: u32,
            pub secondary_maps: u32,
        }

        impl FeatureIndices {
            pub fn to_u32_array(&self) -> [u32; 13] {
                [
                    self.vertex_color_info,
                    self.emissive_strength,
                    self.ior,
                    self.specular,
                    self.transmission,
                    self.diffuse_transmission,
                    self.volume,
                    self.clearcoat,
                    self.sheen,
                    self.dispersion,
                    self.anisotropy,
                    self.iridescence,
                    self.secondary_maps,
                ]
            }
        }
        // First write feature_indices as a placeholder, then go back and fill them in.
        let mut feature_indices = FeatureIndices::default();
        let indices_offset = data.len();
        for value in feature_indices.to_u32_array() {
            data.extend_from_slice(&value.to_le_bytes());
        }

        // Features...
        fn current_index(data: &[u8]) -> u32 {
            let index = data.len() as u32 / 4;
            // subtract 1 for the shader id word
            index - 1
        }

        if let Some(PbrMaterialVertexColorInfo { set_index }) = self.vertex_color_info {
            feature_indices.vertex_color_info = current_index(data);
            write(data, set_index.into());
        }

        if let Some(PbrMaterialEmissiveStrength { strength }) = self.emissive_strength {
            feature_indices.emissive_strength = current_index(data);
            write(data, strength.into());
        }

        if let Some(PbrMaterialIor { ior }) = self.ior {
            feature_indices.ior = current_index(data);
            write(data, ior.into());
        }

        if let Some(PbrMaterialSpecular {
            tex,
            factor,
            color_tex,
            color_factor,
        }) = &self.specular
        {
            feature_indices.specular = current_index(data);

            write_material_texture_or_skip(data, tex.as_ref(), ctx);
            write(data, factor.into());

            write_material_texture_or_skip(data, color_tex.as_ref(), ctx);
            write(data, color_factor[0].into());
            write(data, color_factor[1].into());
            write(data, color_factor[2].into());
        }

        if let Some(PbrMaterialTransmission { tex, factor }) = &self.transmission {
            feature_indices.transmission = current_index(data);

            write_material_texture_or_skip(data, tex.as_ref(), ctx);
            write(data, factor.into());
        }

        if let Some(PbrMaterialDiffuseTransmission {
            tex,
            factor,
            color_tex,
            color_factor,
        }) = &self.diffuse_transmission
        {
            feature_indices.diffuse_transmission = current_index(data);

            write_material_texture_or_skip(data, tex.as_ref(), ctx);
            write(data, factor.into());

            write_material_texture_or_skip(data, color_tex.as_ref(), ctx);
            write(data, color_factor[0].into());
            write(data, color_factor[1].into());
            write(data, color_factor[2].into());
        }

        if let Some(PbrMaterialVolume {
            thickness_tex,
            thickness_factor,
            attenuation_distance,
            attenuation_color,
        }) = &self.volume
        {
            feature_indices.volume = current_index(data);

            write_material_texture_or_skip(data, thickness_tex.as_ref(), ctx);
            write(data, thickness_factor.into());
            write(data, attenuation_distance.into());
            write(data, attenuation_color[0].into());
            write(data, attenuation_color[1].into());
            write(data, attenuation_color[2].into());
        }

        if let Some(PbrMaterialClearCoat {
            tex,
            factor,
            roughness_tex,
            roughness_factor,
            normal_tex,
            normal_scale,
        }) = &self.clearcoat
        {
            feature_indices.clearcoat = current_index(data);

            write_material_texture_or_skip(data, tex.as_ref(), ctx);
            write(data, factor.into());

            write_material_texture_or_skip(data, roughness_tex.as_ref(), ctx);
            write(data, roughness_factor.into());

            write_material_texture_or_skip(data, normal_tex.as_ref(), ctx);
            write(data, normal_scale.into());
        }

        if let Some(PbrMaterialSheen {
            roughness_tex,
            roughness_factor,
            color_tex,
            color_factor,
        }) = &self.sheen
        {
            feature_indices.sheen = current_index(data);

            write_material_texture_or_skip(data, roughness_tex.as_ref(), ctx);
            write(data, roughness_factor.into());

            write_material_texture_or_skip(data, color_tex.as_ref(), ctx);
            write(data, color_factor[0].into());
            write(data, color_factor[1].into());
            write(data, color_factor[2].into());
        }

        if let Some(PbrMaterialDispersion { dispersion }) = self.dispersion {
            feature_indices.dispersion = current_index(data);
            write(data, dispersion.into());
        }

        if let Some(PbrMaterialAnisotropy {
            tex,
            strength,
            rotation,
        }) = &self.anisotropy
        {
            feature_indices.anisotropy = current_index(data);

            write_material_texture_or_skip(data, tex.as_ref(), ctx);
            write(data, strength.into());
            write(data, rotation.into());
        }

        if let Some(PbrMaterialIridescence {
            tex,
            factor,
            ior,
            thickness_tex,
            thickness_min,
            thickness_max,
        }) = &self.iridescence
        {
            feature_indices.iridescence = current_index(data);

            write_material_texture_or_skip(data, tex.as_ref(), ctx);
            write(data, factor.into());
            write(data, ior.into());

            write_material_texture_or_skip(data, thickness_tex.as_ref(), ctx);
            write(data, thickness_min.into());
            write(data, thickness_max.into());
        }

        if let Some(PbrMaterialSecondaryMaps {
            base_color_tex,
            base_color_strength,
            normal_tex,
            normal_strength,
            metallic_roughness_tex,
            metallic_roughness_strength,
            occlusion_tex,
            occlusion_strength,
            emissive_tex,
            emissive_strength,
        }) = &self.secondary_maps
        {
            feature_indices.secondary_maps = current_index(data);

            // 5 slots x 6 words (TextureInfo 5 + strength 1). Unset slots
            // write the zero SKIP sentinel (exists == 0) so the opaque
            // kernel's per-slot branch does no fetch at all.
            write_material_texture_or_skip(data, base_color_tex.as_ref(), ctx);
            write(data, (*base_color_strength).into());

            write_material_texture_or_skip(data, normal_tex.as_ref(), ctx);
            write(data, (*normal_strength).into());

            write_material_texture_or_skip(data, metallic_roughness_tex.as_ref(), ctx);
            write(data, (*metallic_roughness_strength).into());

            write_material_texture_or_skip(data, occlusion_tex.as_ref(), ctx);
            write(data, (*occlusion_strength).into());

            write_material_texture_or_skip(data, emissive_tex.as_ref(), ctx);
            write(data, (*emissive_strength).into());
        }

        // Re-write indices.
        for (index, value) in feature_indices.to_u32_array().iter().enumerate() {
            let start_offset = indices_offset + index * 4;
            let end_offset = start_offset + 4;
            let feature_indices_bytes = &mut data[start_offset..end_offset];
            feature_indices_bytes.copy_from_slice(&value.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod pbr_features_tests {
    use super::*;

    #[test]
    fn bare_material_has_no_features() {
        let m = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
        let f = PbrFeatures::from_material(&m);
        assert_eq!(f, PbrFeatures::default());
        assert_eq!(
            f.bits(),
            0,
            "a no-texture, no-extension PBR material is the smallest feature-set"
        );
    }

    #[test]
    fn extensions_drive_feature_bits() {
        let mut m = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
        m.ior = Some(PbrMaterialIor { ior: 1.5 });
        m.clearcoat = Some(PbrMaterialClearCoat {
            tex: None,
            factor: 1.0,
            roughness_tex: None,
            roughness_factor: 0.0,
            normal_tex: None,
            normal_scale: 1.0,
        });
        m.emissive_strength = Some(PbrMaterialEmissiveStrength { strength: 2.0 });
        let f = PbrFeatures::from_material(&m);
        assert!(f.ior && f.clearcoat && f.emissive_strength);
        assert!(!f.sheen && !f.transmission);
        // Only the three set extensions show up in the hash.
        let expect = PbrFeatures {
            ior: true,
            clearcoat: true,
            emissive_strength: true,
            ..Default::default()
        };
        assert_eq!(f.bits(), expect.bits());
    }

    fn with(edit: impl FnOnce(&mut PbrFeatures)) -> PbrFeatures {
        let mut f = PbrFeatures::default();
        edit(&mut f);
        f
    }

    /// Every single-feature set, by name.
    fn single_features() -> Vec<(&'static str, PbrFeatures)> {
        vec![
            ("vertex_color", with(|f| f.vertex_color = true)),
            ("emissive_strength", with(|f| f.emissive_strength = true)),
            ("ior", with(|f| f.ior = true)),
            ("specular", with(|f| f.specular = true)),
            ("transmission", with(|f| f.transmission = true)),
            ("volume", with(|f| f.volume = true)),
            ("dispersion", with(|f| f.dispersion = true)),
            (
                "diffuse_transmission",
                with(|f| f.diffuse_transmission = true),
            ),
            ("clearcoat", with(|f| f.clearcoat = true)),
            ("sheen", with(|f| f.sheen = true)),
            ("anisotropy", with(|f| f.anisotropy = true)),
            ("iridescence", with(|f| f.iridescence = true)),
            ("secondary_maps", with(|f| f.secondary_maps = true)),
        ]
    }

    const CHEAP: [&str; 3] = ["vertex_color", "emissive_strength", "ior"];
    const NEUTRAL_NOT_FREE: [&str; 4] = ["specular", "transmission", "volume", "dispersion"];
    const LOBES: [&str; 6] = [
        "diffuse_transmission",
        "clearcoat",
        "sheen",
        "anisotropy",
        "iridescence",
        "secondary_maps",
    ];

    /// The default policy: a free feature never gives a material a kernel of
    /// its own; every other feature does.
    #[test]
    fn free_features_do_not_split_kernels_by_default() {
        let policy = PbrSpecialization::default();
        assert_eq!(policy, PbrSpecialization::ExpensiveFeatures);
        let bare = policy.kernel_features(PbrFeatures::default());
        assert_eq!(bare, PbrFeatures::cheap());
        for (name, features) in single_features() {
            let kernel = policy.kernel_features(features);
            if CHEAP.contains(&name) {
                assert_eq!(kernel, bare, "{name} must share the bare kernel");
            } else {
                assert_ne!(kernel, bare, "{name} must keep a kernel of its own");
                assert_eq!(
                    kernel,
                    features.union(PbrFeatures::cheap()),
                    "{name}: its kernel is itself plus the cheap set"
                );
            }
        }
        // Any mix of free features on top of an expensive one lands in that
        // expensive feature's kernel.
        let clearcoat = with(|f| f.clearcoat = true);
        let mixed = with(|f| {
            f.clearcoat = true;
            f.emissive_strength = true;
            f.ior = true;
            f.vertex_color = true;
        });
        assert_eq!(
            policy.kernel_features(clearcoat),
            policy.kernel_features(mixed)
        );
    }

    /// `LobesOnly`: the neutral seven share, the six lobes split.
    #[test]
    fn lobes_only_splits_on_the_six_lobes() {
        let policy = PbrSpecialization::LobesOnly;
        let bare = policy.kernel_features(PbrFeatures::default());
        assert_eq!(bare, PbrFeatures::neutral());
        assert_eq!(bare.bits().count_ones(), 7);
        for (name, features) in single_features() {
            let kernel = policy.kernel_features(features);
            if LOBES.contains(&name) {
                assert_ne!(kernel, bare, "{name} must keep a kernel of its own");
                assert_eq!(kernel, features.union(PbrFeatures::neutral()), "{name}");
            } else {
                assert_eq!(kernel, bare, "{name} must share the bare kernel");
            }
        }
    }

    /// A kernel must never lack code its material needs, under any policy.
    #[test]
    fn kernel_features_are_a_superset_of_the_materials() {
        for policy in [
            PbrSpecialization::ExpensiveFeatures,
            PbrSpecialization::LobesOnly,
            PbrSpecialization::EveryFeature,
        ] {
            for bits in 0..(1u32 << PbrFeatures::COUNT) {
                // Every 97th set keeps this quick; it still covers each bit
                // on and off many times over.
                if bits % 97 != 0 && bits != (1 << PbrFeatures::COUNT) - 1 {
                    continue;
                }
                let material = PbrFeatures::from_bits(bits);
                let kernel = policy.kernel_features(material);
                assert_eq!(
                    kernel.bits() & material.bits(),
                    material.bits(),
                    "{policy:?} dropped a feature of {material:?}"
                );
            }
        }
    }

    /// `EveryFeature` is the pre-0.41 behaviour: the material's own set,
    /// untouched.
    #[test]
    fn every_feature_policy_is_the_identity() {
        let policy = PbrSpecialization::EveryFeature;
        assert_eq!(policy.always_compiled(), PbrFeatures::default());
        for (name, features) in single_features() {
            assert_eq!(policy.kernel_features(features), features, "{name}");
        }
        assert_eq!(
            policy.kernel_features(PbrFeatures::default()),
            PbrFeatures::default()
        );
    }

    /// The sets partition the thirteen features as documented: three free,
    /// four neutral but not free, six lobes.
    #[test]
    fn the_feature_sets_are_what_the_policy_documents() {
        let named = |names: &[&str]| {
            single_features()
                .into_iter()
                .filter(|(name, _)| names.contains(name))
                .fold(PbrFeatures::default(), |acc, (_, f)| acc.union(f))
        };
        assert_eq!(PbrFeatures::cheap(), named(&CHEAP));
        assert_eq!(
            PbrFeatures::neutral(),
            named(&CHEAP).union(named(&NEUTRAL_NOT_FREE))
        );
        assert_eq!(
            PbrFeatures::neutral().bits() & named(&LOBES).bits(),
            0,
            "no lobe is neutral"
        );
        assert_eq!(
            named(&CHEAP)
                .union(named(&NEUTRAL_NOT_FREE))
                .union(named(&LOBES))
                .bits()
                .count_ones(),
            13
        );
        assert!(
            !PbrFeatures::neutral().anisotropy,
            "anisotropy's unused path is not neutral; it must stay specialized"
        );
        for (name, features) in single_features() {
            if NEUTRAL_NOT_FREE.contains(&name) {
                assert_eq!(
                    PbrFeatures::cheap().bits() & features.bits(),
                    0,
                    "{name} costs GPU time when unused; it must not be in the default set"
                );
            }
        }
    }

    #[test]
    fn same_feature_presence_yields_same_hash() {
        // Two materials differing only in scalar factors (not in which
        // slots/extensions are present) MUST land in the same bucket.
        let mut a = PbrMaterial::new(MaterialAlphaMode::Opaque, false);
        a.metallic_factor = 0.2;
        a.ior = Some(PbrMaterialIor { ior: 1.4 });
        let mut b = PbrMaterial::new(MaterialAlphaMode::Opaque, true);
        b.metallic_factor = 0.9;
        b.ior = Some(PbrMaterialIor { ior: 1.9 });
        assert_eq!(
            PbrFeatures::from_material(&a).bits(),
            PbrFeatures::from_material(&b).bits(),
            "feature-hash keys on presence, not scalar values or double_sided"
        );
    }

    #[test]
    fn bits_round_trip_for_all_and_arbitrary() {
        for f in [
            PbrFeatures::default(),
            PbrFeatures::all(),
            PbrFeatures {
                normal_tex: true,
                occlusion_tex: true,
                sheen: true,
                ..Default::default()
            },
        ] {
            assert_eq!(PbrFeatures::from_bits(f.bits()), f);
        }
    }

    #[test]
    fn all_features_set_exactly_count_bits() {
        assert_eq!(PbrFeatures::all().bits().count_ones(), PbrFeatures::COUNT);
        const { assert!(PbrFeatures::COUNT <= 32, "bits() must fit a u32") };
    }
}
