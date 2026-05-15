//! Unlit material — constant emissive surface, no lighting.
//!
//! The WGSL implementation lives in `wgsl/unlit_material.wgsl`. The renderer
//! includes that fragment via the `{{ materials_wgsl }}` askama variable.

use crate::{
    shader::{MaterialShader, TextureSlotDecl},
    writer::{write, write_material_texture},
    MaterialAlphaMode, MaterialShaderId, MaterialTexture, TextureContext,
};

/// WGSL helper module for this material.
pub const WGSL_FRAGMENT: &str = include_str!("wgsl/unlit_material.wgsl");

const TEXTURE_SLOTS: &[TextureSlotDecl] = &[
    TextureSlotDecl { slot_name: "base_color", optional: true },
    TextureSlotDecl { slot_name: "emissive", optional: true },
];

/// Unlit material parameters.
#[derive(Clone, Debug)]
pub struct UnlitMaterial {
    pub base_color_tex: Option<MaterialTexture>,
    pub base_color_factor: [f32; 4],
    pub emissive_tex: Option<MaterialTexture>,
    pub emissive_factor: [f32; 3],
    // Immutable properties — changing them requires recreating the material.
    alpha_mode: MaterialAlphaMode,
    double_sided: bool,
}

impl UnlitMaterial {
    /// Creates an unlit material.
    pub fn new(alpha_mode: MaterialAlphaMode, double_sided: bool) -> Self {
        Self {
            base_color_tex: None,
            base_color_factor: [1.0, 1.0, 1.0, 1.0],
            emissive_tex: None,
            emissive_factor: [0.0, 0.0, 0.0],
            alpha_mode,
            double_sided,
        }
    }

    /// Returns the material alpha mode.
    pub fn alpha_mode_ref(&self) -> &MaterialAlphaMode {
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

impl MaterialShader for UnlitMaterial {
    fn shader_id(&self) -> MaterialShaderId {
        MaterialShaderId::Unlit
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

    fn texture_slots(&self) -> &'static [TextureSlotDecl] {
        TEXTURE_SLOTS
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

        write_material_texture(data, self.emissive_tex.as_ref(), ctx);
        write(data, self.emissive_factor[0].into());
        write(data, self.emissive_factor[1].into());
        write(data, self.emissive_factor[2].into());
    }
}
