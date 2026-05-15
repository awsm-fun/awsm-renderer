//! Alpha mode for materials.

/// Alpha mode for a material. Mirrors the glTF `alphaMode` field but is
/// renderer-internal; the variant indices are committed to WGSL via
/// `variant_as_u32()`.
#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub enum MaterialAlphaMode {
    /// Fully opaque. Renders in the opaque pass.
    #[default]
    Opaque,
    /// Alpha-tested. Fragments with `alpha < cutoff` are discarded. Renders
    /// in the transparency pass.
    Mask {
        /// Discard threshold (typically 0.5).
        cutoff: f32,
    },
    /// Alpha-blended. Renders in the transparency pass.
    Blend,
}

impl MaterialAlphaMode {
    /// Returns the numeric variant index used by WGSL.
    ///
    /// Keep in sync with the `ALPHA_MODE_*` consts in `material.wgsl`.
    pub fn variant_as_u32(&self) -> u32 {
        match self {
            Self::Opaque => 0,
            Self::Mask { .. } => 1,
            Self::Blend => 2,
        }
    }
}
