//! Render-pass shader cache keys.

use crate::render_passes::{
    display::shader::cache_key::ShaderCacheKeyDisplay,
    effects::shader::cache_key::ShaderCacheKeyEffects,
    geometry::shader::cache_key::ShaderCacheKeyGeometry,
    light_culling::shader::cache_key::ShaderCacheKeyLightCulling,
    material_classify::shader::cache_key::ShaderCacheKeyMaterialClassify,
    material_decal::shader::cache_key::ShaderCacheKeyMaterialDecal,
    material_opaque::shader::cache_key::{
        ShaderCacheKeyMaterialOpaque, ShaderCacheKeyMaterialOpaqueEmpty,
    },
    material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent,
};

/// Cache key variants for render-pass shader templates.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub enum ShaderCacheKeyRenderPass {
    Geometry(ShaderCacheKeyGeometry),
    LightCulling(ShaderCacheKeyLightCulling),
    MaterialClassify(ShaderCacheKeyMaterialClassify),
    MaterialDecal(ShaderCacheKeyMaterialDecal),
    MaterialOpaque(ShaderCacheKeyMaterialOpaque),
    MaterialOpaqueEmpty(ShaderCacheKeyMaterialOpaqueEmpty),
    MaterialTransparent(ShaderCacheKeyMaterialTransparent),
    Effects(ShaderCacheKeyEffects),
    Display(ShaderCacheKeyDisplay),
}
