//! Render-pass shader cache keys.

#[cfg(feature = "lod")]
use crate::render_passes::cluster_lod::shader::cache_key::{
    ShaderCacheKeyClusterCompaction, ShaderCacheKeyClusterCut,
};
use crate::render_passes::{
    bloom::shader::cache_key::{ShaderCacheKeyBloomCombine, ShaderCacheKeyBloomDownsample},
    coverage::shader::cache_key::ShaderCacheKeyCoverage,
    display::shader::cache_key::ShaderCacheKeyDisplay,
    effects::shader::cache_key::ShaderCacheKeyEffects,
    geometry::shader::cache_key::ShaderCacheKeyGeometry,
    geometry::shader::custom_vertex_cache_key::ShaderCacheKeyGeometryCustomVertex,
    geometry::shader::masked_cache_key::ShaderCacheKeyGeometryMasked,
    geometry::shader::masked_custom_vertex_cache_key::ShaderCacheKeyGeometryMaskedCustomVertex,
    hzb::shader::cache_key::{ShaderCacheKeyHzbReduce, ShaderCacheKeyHzbSeed},
    light_culling::shader::cache_key::ShaderCacheKeyLightCulling,
    material_classify::shader::cache_key::ShaderCacheKeyMaterialClassify,
    material_decal::classify::shader::cache_key::ShaderCacheKeyDecalClassify,
    material_decal::shader::cache_key::ShaderCacheKeyMaterialDecal,
    material_opaque::shader::cache_key::ShaderCacheKeyMaterialOpaque,
    material_opaque::shader::edge_cache_key::ShaderCacheKeyMaterialFinalBlend,
    material_prep::shader::cache_key::{ShaderCacheKeyMaterialPrep, ShaderCacheKeyShadowBlur},
    material_transparent::shader::cache_key::ShaderCacheKeyMaterialTransparent,
    occlusion::shader::cache_key::{
        ShaderCacheKeyOcclusionCompaction, ShaderCacheKeyOcclusionCull,
    },
    ssr::shader::cache_key::ShaderCacheKeySsr,
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
    /// Combined masked (alpha-tested) + custom-vertex (displacement) variant of
    /// the geometry raster — per-`shader_id` specialized; see
    /// [`ShaderCacheKeyGeometryMaskedCustomVertex`].
    GeometryMaskedCustomVertex(ShaderCacheKeyGeometryMaskedCustomVertex),
    HzbSeed(ShaderCacheKeyHzbSeed),
    HzbReduce(ShaderCacheKeyHzbReduce),
    /// Bloom pyramid down-sample (prefilter + plain 13-tap variants).
    BloomDownsample(ShaderCacheKeyBloomDownsample),
    /// Bloom mip-sum combine → full-res bloom.
    BloomCombine(ShaderCacheKeyBloomCombine),
    LightCulling(ShaderCacheKeyLightCulling),
    MaterialClassify(ShaderCacheKeyMaterialClassify),
    /// Plan B shared prep pass (docs/plans/deferred-shared-prep-pass.md).
    MaterialPrep(ShaderCacheKeyMaterialPrep),
    /// Optional shadow-visibility denoise blur (`cs_blur_h` / `cs_blur_v`).
    ShadowBlur(ShaderCacheKeyShadowBlur),
    DecalClassify(ShaderCacheKeyDecalClassify),
    MaterialDecal(ShaderCacheKeyMaterialDecal),
    MaterialOpaque(ShaderCacheKeyMaterialOpaque),
    /// Global final-blend compositor for the MSAA edge-resolve flow.
    MaterialFinalBlend(ShaderCacheKeyMaterialFinalBlend),
    MaterialTransparent(ShaderCacheKeyMaterialTransparent),
    OcclusionCull(ShaderCacheKeyOcclusionCull),
    OcclusionCompaction(ShaderCacheKeyOcclusionCompaction),
    /// Cluster-LOD per-cluster cut compute (Phase B, B.2; `virtual_geometry`).
    #[cfg(feature = "lod")]
    ClusterCut(ShaderCacheKeyClusterCut),
    /// Cluster-LOD compaction compute (Phase B, B.2; `virtual_geometry`).
    #[cfg(feature = "lod")]
    ClusterCompaction(ShaderCacheKeyClusterCompaction),
    Effects(ShaderCacheKeyEffects),
    Display(ShaderCacheKeyDisplay),
    /// Screen-space reflections trace (linear-DDA march). Permutes on mode
    /// (mirror/glossy) × temporal × half-res.
    Ssr(ShaderCacheKeySsr),
}
