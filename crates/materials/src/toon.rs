//! Toon material — banded diffuse + stepped Blinn-Phong specular + rim.
//!
//! Authored knobs:
//! - `base_color_factor` — surface tint.
//! - `emissive_factor` — additive emissive term (added after lighting).
//! - `diffuse_bands` — number of quantization steps for diffuse N·L (≥ 1).
//!   Typical: 2 (hard half-Lambert), 3 (classic cel), 4+ (soft).
//! - `specular_steps` — quantization steps for the Blinn-Phong specular term.
//! - `shininess` — Blinn-Phong exponent. Higher = tighter highlight.
//! - `rim_strength` — additive rim term (0 disables).
//! - `rim_power` — exponent on the silhouette `1 - N·V` term.
//!
//! All shading happens in `compute_toon_lit_color` (WGSL), which mirrors
//! `apply_lighting`'s signature so the visibility-buffer compute pass can
//! drop it in alongside PBR / Unlit.

use crate::{
    shader::MaterialShader,
    writer::{write, write_material_texture},
    MaterialAlphaMode, MaterialShaderId, MaterialTexture, TextureContext,
};

/// WGSL helper module for this material.
pub const WGSL_FRAGMENT: &str = include_str!("wgsl/toon_material.wgsl");

/// Toon material parameters.
#[derive(Clone, Debug)]
pub struct ToonMaterial {
    pub base_color_tex: Option<MaterialTexture>,
    pub base_color_factor: [f32; 4],
    pub emissive_tex: Option<MaterialTexture>,
    pub emissive_factor: [f32; 3],
    pub diffuse_bands: u32,
    pub specular_steps: u32,
    pub shininess: f32,
    pub rim_strength: f32,
    pub rim_power: f32,
    alpha_mode: MaterialAlphaMode,
    double_sided: bool,
}

impl ToonMaterial {
    /// Creates a toon material with sensible default knobs (3-band diffuse,
    /// 2-step spec, classic cel highlight).
    pub fn new(alpha_mode: MaterialAlphaMode, double_sided: bool) -> Self {
        Self {
            base_color_tex: None,
            base_color_factor: [1.0, 1.0, 1.0, 1.0],
            emissive_tex: None,
            emissive_factor: [0.0, 0.0, 0.0],
            diffuse_bands: 3,
            specular_steps: 2,
            shininess: 32.0,
            rim_strength: 0.4,
            rim_power: 2.0,
            alpha_mode,
            double_sided,
        }
    }

    pub fn alpha_mode(&self) -> &MaterialAlphaMode {
        &self.alpha_mode
    }

    pub fn double_sided(&self) -> bool {
        self.double_sided
    }

    pub fn alpha_cutoff(&self) -> Option<f32> {
        match self.alpha_mode {
            MaterialAlphaMode::Mask { cutoff } => Some(cutoff),
            _ => None,
        }
    }

    pub fn has_alpha_blend(&self) -> bool {
        matches!(self.alpha_mode, MaterialAlphaMode::Blend)
    }
}

/// Feature set for the Toon shading family — the Toon analogue of
/// [`crate::pbr::PbrFeatures`], over [`ToonMaterial`]'s optional fields.
///
/// **Status (specialize-only pivot):** Toon's v1 shading
/// (`compute_toon_lit_color`) does NOT sample its optional textures —
/// `base_color_tex` / `emissive_tex` are written to the payload but unused
/// by the shader. So Toon currently has **no compile-gateable shading
/// paths**, and the unified variant registry correctly resolves it to a
/// **single bucket** (its one empty feature-set), which is the optimal
/// specialization outcome for a family with no per-feature variation
/// (`ShadingBase::Toon.is_feature_specialized()` stays `false`).
///
/// This struct is the ready-to-use contract for when Toon gains texture
/// sampling: at that point, gate the toon shading on `{% if
/// toon_features.<x> %}` (move the shading into an Askama-processed
/// include, mirroring PBR's `material_color_calc.wgsl`), flip
/// `is_feature_specialized`, and add `Material::Toon` to the renderer's
/// variant reconcile — the registry + routing already handle any
/// `FirstParty` base generically.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ToonFeatures {
    pub base_color_tex: bool,
    pub emissive_tex: bool,
}

impl ToonFeatures {
    const BIT_BASE_COLOR_TEX: u32 = 1 << 0;
    const BIT_EMISSIVE_TEX: u32 = 1 << 1;

    /// Number of distinct feature bits.
    pub const COUNT: u32 = 2;

    /// Derives the feature set from a material's present `Option` fields.
    pub fn from_material(m: &ToonMaterial) -> Self {
        Self {
            base_color_tex: m.base_color_tex.is_some(),
            emissive_tex: m.emissive_tex.is_some(),
        }
    }

    /// Every feature on — the uber config.
    pub fn all() -> Self {
        Self {
            base_color_tex: true,
            emissive_tex: true,
        }
    }

    /// Stable one-bit-per-feature packing (the feature-hash). Append-only.
    pub fn bits(&self) -> u32 {
        let mut b = 0u32;
        if self.base_color_tex {
            b |= Self::BIT_BASE_COLOR_TEX;
        }
        if self.emissive_tex {
            b |= Self::BIT_EMISSIVE_TEX;
        }
        b
    }

    /// Inverse of [`Self::bits`].
    pub fn from_bits(b: u32) -> Self {
        Self {
            base_color_tex: b & Self::BIT_BASE_COLOR_TEX != 0,
            emissive_tex: b & Self::BIT_EMISSIVE_TEX != 0,
        }
    }
}

impl MaterialShader for ToonMaterial {
    fn shader_id(&self) -> MaterialShaderId {
        MaterialShaderId::TOON
    }

    fn wgsl_fragment(&self) -> &'static str {
        WGSL_FRAGMENT
    }

    fn alpha_mode(&self) -> MaterialAlphaMode {
        self.alpha_mode
    }

    fn is_transparency_pass(&self) -> bool {
        self.has_alpha_blend() || self.alpha_cutoff().is_some()
    }

    /// Layout must stay in sync with `toon_get_material` in WGSL.
    fn write_uniform_buffer(&self, ctx: &dyn TextureContext, data: &mut Vec<u8>) {
        write(data, self.shader_id().as_u32().into());

        write(data, self.alpha_mode.variant_as_u32().into());
        write(data, self.alpha_cutoff().unwrap_or(0.0f32).into());

        write_material_texture(data, self.base_color_tex.as_ref(), ctx);
        write(data, self.base_color_factor[0].into());
        write(data, self.base_color_factor[1].into());
        write(data, self.base_color_factor[2].into());
        write(data, self.base_color_factor[3].into());

        write_material_texture(data, self.emissive_tex.as_ref(), ctx);
        write(data, self.emissive_factor[0].into());
        write(data, self.emissive_factor[1].into());
        write(data, self.emissive_factor[2].into());

        write(data, self.diffuse_bands.into());
        write(data, self.specular_steps.into());
        write(data, self.shininess.into());
        write(data, self.rim_strength.into());
        write(data, self.rim_power.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MaterialAlphaMode;

    #[test]
    fn toon_features_bits_round_trip() {
        for mask in 0u32..(1 << ToonFeatures::COUNT) {
            let f = ToonFeatures::from_bits(mask);
            assert_eq!(
                f.bits(),
                mask,
                "from_bits→bits must round-trip for {mask:#b}"
            );
        }
    }

    #[test]
    fn toon_features_from_material_derives_optional_fields() {
        // Default toon material has no textures → empty feature-set →
        // single bucket (the correct specialization outcome).
        let m = ToonMaterial::new(MaterialAlphaMode::Opaque, false);
        let f = ToonFeatures::from_material(&m);
        assert!(!f.base_color_tex && !f.emissive_tex);
        assert_eq!(
            f.bits(),
            0,
            "a textureless toon material is the empty feature-set"
        );
        assert_eq!(ToonFeatures::all().bits(), 0b11);
    }
}
