//! Authored sprite / billboard nodes.

use super::primitive::TextureRef;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Eq, Hash, Copy, Default)]
pub enum BillboardMode {
    /// No billboarding — the quad sits in 3D space as authored.
    None,
    /// Rotate around the world Y axis to face the camera (signage, name tags).
    YAxis,
    /// Fully face the camera (round particle stand-ins).
    #[default]
    Full,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Copy, Default)]
pub enum SpriteAlphaMode {
    Opaque,
    Mask {
        cutoff_x1000: u32,
    },
    #[default]
    Blend,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SpriteDef {
    pub texture: Option<TextureRef>,
    pub size: [f32; 2],
    pub billboard: BillboardMode,
    pub alpha_mode: SpriteAlphaMode,
    pub tint: [f32; 4],
}

impl Default for SpriteDef {
    fn default() -> Self {
        Self {
            texture: None,
            size: [1.0, 1.0],
            billboard: BillboardMode::default(),
            alpha_mode: SpriteAlphaMode::default(),
            tint: [1.0, 1.0, 1.0, 1.0],
        }
    }
}
