//! Line shader cache key.

use crate::shaders::ShaderCacheKey;

/// Cache key for the fat-line WGSL shader. Static source — no
/// per-variant parameters — so a zero-size struct is enough.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyLine {
    /// TAA: also write the reactive mask (`@location(1)`, `alpha *
    /// k_transparent`, 03 §5.1). `false` is exactly the pre-TAA shader.
    pub reactive: bool,
}

impl From<ShaderCacheKeyLine> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyLine) -> Self {
        ShaderCacheKey::Line(key)
    }
}
