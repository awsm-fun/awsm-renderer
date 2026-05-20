//! Authored decal configuration. The runtime equivalent lives in
//! `awsm_renderer::decals::Decal`; the editor's renderer bridge
//! resolves the texture ref and pushes a runtime decal via
//! `AwsmRenderer::insert_decal` (Cluster 6.4 / plan §16.4).
//!
//! The decal is an *oriented unit cube* in world space — the node's
//! transform supplies position / orientation / size. Local-space xy
//! maps onto the texture; the decal projects down its local -Z axis.

use super::primitive::TextureRef;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DecalConfig {
    /// Texture asset projected onto the geometry under the decal cube.
    /// `None` keeps the decal inert — useful while authoring before a
    /// texture is wired up.
    #[serde(default)]
    pub texture: Option<TextureRef>,
    /// Global alpha multiplier applied on top of the texture's authored
    /// alpha. `1.0` uses the texture's alpha verbatim.
    #[serde(default = "default_alpha")]
    pub alpha: f32,
    /// Blend accumulation mode. v1 ships alpha-blend only; the enum
    /// reserves room for additive / multiply.
    #[serde(default)]
    pub blend_mode: DecalBlendMode,
}

impl Default for DecalConfig {
    fn default() -> Self {
        Self {
            texture: None,
            alpha: 1.0,
            blend_mode: DecalBlendMode::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecalBlendMode {
    #[default]
    AlphaBlend,
}

fn default_alpha() -> f32 {
    1.0
}
