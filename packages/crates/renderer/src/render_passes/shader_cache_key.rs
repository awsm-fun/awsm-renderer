//! Render-pass shader cache keys.

use crate::render_passes::{
    coverage::shader::cache_key::ShaderCacheKeyCoverage,
    display::shader::cache_key::ShaderCacheKeyDisplay,
    effects::shader::cache_key::ShaderCacheKeyEffects,
    geometry::shader::cache_key::ShaderCacheKeyGeometry,
    geometry::shader::custom_vertex_cache_key::ShaderCacheKeyGeometryCustomVertex,
    geometry::shader::masked_cache_key::ShaderCacheKeyGeometryMasked,
    hzb::shader::cache_key::{ShaderCacheKeyHzbReduce, ShaderCacheKeyHzbSeed},
    light_culling::shader::cache_key::ShaderCacheKeyLightCulling,
    material_classify::shader::cache_key::ShaderCacheKeyMaterialClassify,
    material_decal::classify::shader::cache_key::ShaderCacheKeyDecalClassify,
    material_decal::shader::cache_key::ShaderCacheKeyMaterialDecal,
    material_opaque::shader::cache_key::ShaderCacheKeyMaterialOpaque,
    material_opaque::shader::edge_cache_key::ShaderCacheKeyMaterialFinalBlend,
    material_prep::shader::cache_key::ShaderCacheKeyMaterialPrep,
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
    /// Masked (alpha-tested) variant of the geometry raster — per-`shader_id`
    /// specialized; see [`ShaderCacheKeyGeometryMasked`].
    GeometryMasked(ShaderCacheKeyGeometryMasked),
    /// Custom-vertex (programmable displacement) variant of the geometry
    /// raster — per-`shader_id` specialized; see
    /// [`ShaderCacheKeyGeometryCustomVertex`].
    GeometryCustomVertex(ShaderCacheKeyGeometryCustomVertex),
    HzbSeed(ShaderCacheKeyHzbSeed),
    HzbReduce(ShaderCacheKeyHzbReduce),
    LightCulling(ShaderCacheKeyLightCulling),
    MaterialClassify(ShaderCacheKeyMaterialClassify),
    /// Plan B shared prep pass (docs/plans/deferred-shared-prep-pass.md).
    MaterialPrep(ShaderCacheKeyMaterialPrep),
    DecalClassify(ShaderCacheKeyDecalClassify),
    MaterialDecal(ShaderCacheKeyMaterialDecal),
    MaterialOpaque(ShaderCacheKeyMaterialOpaque),
    /// Global final-blend compositor for the MSAA edge-resolve flow.
    MaterialFinalBlend(ShaderCacheKeyMaterialFinalBlend),
    MaterialTransparent(ShaderCacheKeyMaterialTransparent),
    OcclusionCull(ShaderCacheKeyOcclusionCull),
    OcclusionCompaction(ShaderCacheKeyOcclusionCompaction),
    Effects(ShaderCacheKeyEffects),
    Display(ShaderCacheKeyDisplay),
}
