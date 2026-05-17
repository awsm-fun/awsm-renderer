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

impl MaterialShader for ToonMaterial {
    fn shader_id(&self) -> MaterialShaderId {
        MaterialShaderId::Toon
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
