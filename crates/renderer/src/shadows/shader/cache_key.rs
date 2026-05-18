//! Shader cache key for shadow generation shaders.

use crate::shaders::ShaderCacheKey;

/// Cache key identifying a unique shadow-generation shader variant.
///
/// One variant per instancing mode. The shader has no fragment stage,
/// so MSAA / color-target options don't fork the WGSL source.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ShaderCacheKeyShadow {
    /// Whether the pipeline reads the per-instance transform vertex
    /// buffer at vertex-buffer slot 1. Must match the pipeline's
    /// vertex-buffer layout.
    pub instancing_transforms: bool,
}

impl From<ShaderCacheKeyShadow> for ShaderCacheKey {
    fn from(value: ShaderCacheKeyShadow) -> Self {
        ShaderCacheKey::Shadow(value)
    }
}
