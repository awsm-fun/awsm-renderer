//! Toon material parameters: banded diffuse + stepped Blinn-Phong specular + rim.
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
    materials::{
        writer::{write, Value},
        MaterialAlphaMode, MaterialShaderId, MaterialTexture, Result,
    },
    textures::{SamplerKey, Textures},
};

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

    /// Returns true if the material should render in the transparency pass.
    pub fn is_transparency_pass(&self) -> bool {
        self.has_alpha_blend() || self.alpha_cutoff().is_some()
    }

    pub fn alpha_mode(&self) -> &MaterialAlphaMode {
        &self.alpha_mode
    }

    pub fn double_sided(&self) -> bool {
        self.double_sided
    }

    pub fn alpha_cutoff(&self) -> Option<f32> {
        match self.alpha_mode() {
            MaterialAlphaMode::Mask { cutoff } => Some(*cutoff),
            _ => None,
        }
    }

    pub fn has_alpha_blend(&self) -> bool {
        matches!(self.alpha_mode(), MaterialAlphaMode::Blend)
    }

    pub fn alpha_mask(&self) -> Option<f32> {
        match self.alpha_mode() {
            MaterialAlphaMode::Mask { cutoff } => Some(*cutoff),
            _ => None,
        }
    }

    /// Builds the uniform buffer payload for this material.
    /// Layout must stay in sync with `toon_get_material` in WGSL.
    pub fn uniform_buffer_data(&self, textures: &Textures) -> Result<Vec<u8>> {
        let mut data: Vec<u8> = Vec::with_capacity(128);

        let sampler_key_list: Vec<SamplerKey> = textures.pool_sampler_set.iter().cloned().collect();
        let map_texture = |tex: &MaterialTexture| {
            crate::materials::writer::map_texture(tex, textures, &sampler_key_list)
        };

        write(&mut data, (MaterialShaderId::Toon as u32).into());

        write(&mut data, self.alpha_mode().variant_as_u32().into());
        write(&mut data, self.alpha_cutoff().unwrap_or(0.0f32).into());

        if let Some(tex) = self.base_color_tex.as_ref().and_then(map_texture) {
            write(&mut data, tex);
        } else {
            write(&mut data, Value::SkipTexture);
        }
        write(&mut data, self.base_color_factor[0].into());
        write(&mut data, self.base_color_factor[1].into());
        write(&mut data, self.base_color_factor[2].into());
        write(&mut data, self.base_color_factor[3].into());

        if let Some(tex) = self.emissive_tex.as_ref().and_then(map_texture) {
            write(&mut data, tex);
        } else {
            write(&mut data, Value::SkipTexture);
        }
        write(&mut data, self.emissive_factor[0].into());
        write(&mut data, self.emissive_factor[1].into());
        write(&mut data, self.emissive_factor[2].into());

        write(&mut data, self.diffuse_bands.into());
        write(&mut data, self.specular_steps.into());
        write(&mut data, self.shininess.into());
        write(&mut data, self.rim_strength.into());
        write(&mut data, self.rim_power.into());

        Ok(data)
    }
}
