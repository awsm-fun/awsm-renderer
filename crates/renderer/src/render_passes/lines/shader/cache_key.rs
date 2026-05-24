//! Line shader cache key.

use crate::shaders::ShaderCacheKey;

/// Cache key for the fat-line WGSL shader. Static source — no
/// per-variant parameters — so a zero-size struct is enough.
#[derive(Hash, Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaderCacheKeyLine;

impl From<ShaderCacheKeyLine> for ShaderCacheKey {
    fn from(key: ShaderCacheKeyLine) -> Self {
        ShaderCacheKey::Line(key)
    }
}
