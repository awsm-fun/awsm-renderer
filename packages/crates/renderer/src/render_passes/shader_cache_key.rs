//! Render-pass shader cache keys.

use crate::render_passes::{
    coverage::shader::cache_key::ShaderCacheKeyCoverage,
    display::shader::cache_key::ShaderCacheKeyDisplay,
    effects::shader::cache_key::ShaderCacheKeyEffects,
    geometry::shader::cache_key::ShaderCacheKeyGeometry,
    hzb::shader::cache_key::{ShaderCacheKeyHzbReduce, ShaderCacheKeyHzbSeed},
    light_culling::shader::cache_key::ShaderCacheKeyLightCulling,
    material_classify::shader::cache_key::ShaderCacheKeyMaterialClassify,
    material_decal::classify::shader::cache_key::ShaderCacheKeyDecalClassify,
    material_decal::shader::cache_key::ShaderCacheKeyMaterialDecal,
    material_opaque::shader::cache_key::{
        ShaderCacheKeyMaterialOpaque, ShaderCacheKeyMaterialOpaqueEmpty,
    },
    material_opaque::shader::edge_cache_key::{
        ShaderCacheKeyMaterialEdgeResolve, ShaderCacheKeyMaterialFinalBlend,
        ShaderCacheKeyMaterialSkyboxEdgeResolve,
    },
    material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent,
    occlusion::shader::cache_key::{
        ShaderCacheKeyOcclusionCompaction, ShaderCacheKeyOcclusionCull,
    },
};

/// Cache key variants for render-pass shader templates.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub enum ShaderCacheKeyRenderPass {
    Coverage(ShaderCacheKeyCoverage),
    Geometry(ShaderCacheKeyGeometry),
    HzbSeed(ShaderCacheKeyHzbSeed),
    HzbReduce(ShaderCacheKeyHzbReduce),
    LightCulling(ShaderCacheKeyLightCulling),
    MaterialClassify(ShaderCacheKeyMaterialClassify),
    DecalClassify(ShaderCacheKeyDecalClassify),
    MaterialDecal(ShaderCacheKeyMaterialDecal),
    MaterialOpaque(ShaderCacheKeyMaterialOpaque),
    MaterialOpaqueEmpty(ShaderCacheKeyMaterialOpaqueEmpty),
    /// Per-shader-id MSAA edge-resolve — see Priority 3 in
    /// https://github.com/dakom/awsm-renderer/pull/99.
    MaterialEdgeResolve(ShaderCacheKeyMaterialEdgeResolve),
    /// Global skybox-sample MSAA edge-resolve.
    MaterialSkyboxEdgeResolve(ShaderCacheKeyMaterialSkyboxEdgeResolve),
    /// Global final-blend compositor for the MSAA edge-resolve flow.
    MaterialFinalBlend(ShaderCacheKeyMaterialFinalBlend),
    MaterialTransparent(ShaderCacheKeyMaterialTransparent),
    OcclusionCull(ShaderCacheKeyOcclusionCull),
    OcclusionCompaction(ShaderCacheKeyOcclusionCompaction),
    Effects(ShaderCacheKeyEffects),
    Display(ShaderCacheKeyDisplay),
}
