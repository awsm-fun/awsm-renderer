//! Physically based rendering (PBR) material parameters and packing.

use crate::{
    shader::MaterialShader,
    writer::{write, write_material_texture},
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

    // Things that affect shader generation and therefore can't be changed
    // dynamically — create a new material instead.
    alpha_mode: MaterialAlphaMode,
    double_sided: bool,
}

/// Compile-time feature set of a PBR material: which optional code paths
/// its specialized shader actually needs. Derived from a [`PbrMaterial`]'s
/// present texture slots + extensions (the `Option` fields). This is the
/// input to:
///
/// 1. **B.2** — the Askama `{% if features.<x> %}` gating in the PBR
///    shader (opaque compute + transparent fragment), so a material with
///    (say) no normal map compiles no normal-map code.
/// 2. **B.3** — the per-feature-set bucket id: [`Self::bits`] is the
///    stable feature-hash that maps a feature-set to its own `shader_id`
///    / bucket, so two materials with the same feature-set share one
///    specialized pipeline.
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

    /// The number of distinct feature bits (≤ 32 so [`Self::bits`] fits a u32).
    pub const COUNT: u32 = 17;

    /// Derives the feature set actually used by a material.
    pub fn from_material(m: &PbrMaterial) -> Self {
        Self {
            base_color_tex: m.base_color_tex.is_some(),
            metallic_roughness_tex: m.metallic_roughness_tex.is_some(),
            normal_tex: m.normal_tex.is_some(),
            occlusion_tex: m.occlusion_tex.is_some(),
            emissive_tex: m.emissive_tex.is_some(),
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
        }
    }

    /// Every feature on — the canonical all-features config. Used as the
    /// `pbr_specialization=false` compat path, the canonical first-party
    /// bucket id, and the cap-guard fallback. Rendering with this is
    /// behaviourally identical to the pre-specialization (always-all-
    /// extensions) PBR shader, which is what made the B.2 templatization
    /// landable as a no-op first.
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
        }
    }

    /// Stable one-bit-per-feature packing. Doubles as the feature-hash
    /// keying a feature-set to its opaque bucket (B.3).
    pub fn bits(&self) -> u32 {
        let mut b = 0u32;
        if self.base_color_tex { b |= Self::BIT_BASE_COLOR_TEX; }
        if self.metallic_roughness_tex { b |= Self::BIT_METALLIC_ROUGHNESS_TEX; }
        if self.normal_tex { b |= Self::BIT_NORMAL_TEX; }
        if self.occlusion_tex { b |= Self::BIT_OCCLUSION_TEX; }
        if self.emissive_tex { b |= Self::BIT_EMISSIVE_TEX; }
        if self.vertex_color { b |= Self::BIT_VERTEX_COLOR; }
        if self.emissive_strength { b |= Self::BIT_EMISSIVE_STRENGTH; }
        if self.ior { b |= Self::BIT_IOR; }
        if self.specular { b |= Self::BIT_SPECULAR; }
        if self.transmission { b |= Self::BIT_TRANSMISSION; }
        if self.diffuse_transmission { b |= Self::BIT_DIFFUSE_TRANSMISSION; }
        if self.volume { b |= Self::BIT_VOLUME; }
        if self.clearcoat { b |= Self::BIT_CLEARCOAT; }
        if self.sheen { b |= Self::BIT_SHEEN; }
        if self.dispersion { b |= Self::BIT_DISPERSION; }
        if self.anisotropy { b |= Self::BIT_ANISOTROPY; }
        if self.iridescence { b |= Self::BIT_IRIDESCENCE; }
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
        }
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
            debug: PbrMaterialDebug::None,
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

    /// Returns true if alpha blending is enabled.
    pub fn has_alpha_blend(&self) -> bool {
        matches!(self.alpha_mode, MaterialAlphaMode::Blend)
    }
}

impl MaterialShader for PbrMaterial {
    fn shader_id(&self) -> MaterialShaderId {
        MaterialShaderId::PBR
    }

    fn wgsl_fragment(&self) -> &'static str {
        WGSL_FRAGMENT
    }

    fn alpha_mode(&self) -> MaterialAlphaMode {
        self.alpha_mode
    }

    fn is_transparency_pass(&self) -> bool {
        self.has_alpha_blend() || self.alpha_cutoff().is_some() || self.has_transmission()
    }

    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, data: &mut Vec<u8>) {
        write(data, self.shader_id().as_u32().into());

        write(data, self.alpha_mode.variant_as_u32().into());
        write(data, self.alpha_cutoff().unwrap_or(0.0f32).into());

        write_material_texture(data, self.base_color_tex.as_ref(), ctx);
        write(data, self.base_color_factor[0].into());
        write(data, self.base_color_factor[1].into());
        write(data, self.base_color_factor[2].into());
        write(data, self.base_color_factor[3].into());

        write_material_texture(data, self.metallic_roughness_tex.as_ref(), ctx);
        write(data, self.metallic_factor.into());
        write(data, self.roughness_factor.into());

        write_material_texture(data, self.normal_tex.as_ref(), ctx);
        write(data, self.normal_scale.into());

        write_material_texture(data, self.occlusion_tex.as_ref(), ctx);
        write(data, self.occlusion_strength.into());

        write_material_texture(data, self.emissive_tex.as_ref(), ctx);
        write(data, self.emissive_factor[0].into());
        write(data, self.emissive_factor[1].into());
        write(data, self.emissive_factor[2].into());

        write(data, self.debug.bitmask().into());

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
        }

        impl FeatureIndices {
            pub fn to_u32_array(&self) -> [u32; 12] {
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

            write_material_texture(data, tex.as_ref(), ctx);
            write(data, factor.into());

            write_material_texture(data, color_tex.as_ref(), ctx);
            write(data, color_factor[0].into());
            write(data, color_factor[1].into());
            write(data, color_factor[2].into());
        }

        if let Some(PbrMaterialTransmission { tex, factor }) = &self.transmission {
            feature_indices.transmission = current_index(data);

            write_material_texture(data, tex.as_ref(), ctx);
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

            write_material_texture(data, tex.as_ref(), ctx);
            write(data, factor.into());

            write_material_texture(data, color_tex.as_ref(), ctx);
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

            write_material_texture(data, thickness_tex.as_ref(), ctx);
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

            write_material_texture(data, tex.as_ref(), ctx);
            write(data, factor.into());

            write_material_texture(data, roughness_tex.as_ref(), ctx);
            write(data, roughness_factor.into());

            write_material_texture(data, normal_tex.as_ref(), ctx);
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

            write_material_texture(data, roughness_tex.as_ref(), ctx);
            write(data, roughness_factor.into());

            write_material_texture(data, color_tex.as_ref(), ctx);
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

            write_material_texture(data, tex.as_ref(), ctx);
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

            write_material_texture(data, tex.as_ref(), ctx);
            write(data, factor.into());
            write(data, ior.into());

            write_material_texture(data, thickness_tex.as_ref(), ctx);
            write(data, thickness_min.into());
            write(data, thickness_max.into());
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
        assert_eq!(f.bits(), 0, "a no-texture, no-extension PBR material is the smallest feature-set");
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
        let expect = PbrFeatures { ior: true, clearcoat: true, emissive_strength: true, ..Default::default() };
        assert_eq!(f.bits(), expect.bits());
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
        for f in [PbrFeatures::default(), PbrFeatures::all(),
                  PbrFeatures { normal_tex: true, occlusion_tex: true, sheen: true, ..Default::default() }] {
            assert_eq!(PbrFeatures::from_bits(f.bits()), f);
        }
    }

    #[test]
    fn all_features_set_exactly_count_bits() {
        assert_eq!(PbrFeatures::all().bits().count_ones(), PbrFeatures::COUNT);
        assert!(PbrFeatures::COUNT <= 32, "bits() must fit a u32");
    }
}
