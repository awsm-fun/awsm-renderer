//! Shader template variants for render passes.

#[cfg(feature = "lod")]
use crate::render_passes::cluster_lod::shader::template::{
    ShaderTemplateClusterCompaction, ShaderTemplateClusterCut,
};
use crate::{
    render_passes::{
        bloom::shader::template::{ShaderTemplateBloomCombine, ShaderTemplateBloomDownsample},
        coverage::shader::template::ShaderTemplateCoverage,
        display::shader::template::ShaderTemplateDisplay,
        effects::shader::template::ShaderTemplateEffects,
        geometry::shader::custom_vertex_template::ShaderTemplateGeometryCustomVertex,
        geometry::shader::masked_custom_vertex_template::ShaderTemplateGeometryMaskedCustomVertex,
        geometry::shader::masked_template::ShaderTemplateGeometryMasked,
        geometry::shader::template::ShaderTemplateGeometry,
        hzb::shader::template::{ShaderTemplateHzbReduce, ShaderTemplateHzbSeed},
        light_culling::shader::template::ShaderTemplateLightCulling,
        material_classify::shader::template::ShaderTemplateMaterialClassify,
        material_decal::classify::shader::template::ShaderTemplateDecalClassify,
        material_decal::shader::template::ShaderTemplateMaterialDecal,
        material_opaque::shader::edge_template::ShaderTemplateMaterialFinalBlend,
        material_opaque::shader::template::ShaderTemplateMaterialOpaque,
        material_prep::shader::template::{ShaderTemplateMaterialPrep, ShaderTemplateShadowBlur},
        material_transparent::shader::template::ShaderTemplateMaterialTransparent,
        occlusion::shader::template::{
            ShaderTemplateOcclusionCompaction, ShaderTemplateOcclusionCull,
        },
        shader_cache_key::ShaderCacheKeyRenderPass,
        ssr::shader::template::ShaderTemplateSsr,
        ssr_minz::shader::template::{ShaderTemplateSsrMinzReduce, ShaderTemplateSsrMinzSeed},
    },
    shaders::AwsmShaderError,
};

/// Render-pass shader template variants.
pub enum ShaderTemplateRenderPass {
    Coverage(ShaderTemplateCoverage),
    Geometry(ShaderTemplateGeometry),
    GeometryMasked(ShaderTemplateGeometryMasked),
    GeometryCustomVertex(ShaderTemplateGeometryCustomVertex),
    GeometryMaskedCustomVertex(ShaderTemplateGeometryMaskedCustomVertex),
    HzbSeed(ShaderTemplateHzbSeed),
    HzbReduce(ShaderTemplateHzbReduce),
    BloomDownsample(ShaderTemplateBloomDownsample),
    BloomCombine(ShaderTemplateBloomCombine),
    LightCulling(ShaderTemplateLightCulling),
    MaterialClassify(ShaderTemplateMaterialClassify),
    MaterialPrep(ShaderTemplateMaterialPrep),
    ShadowBlur(ShaderTemplateShadowBlur),
    DecalClassify(ShaderTemplateDecalClassify),
    MaterialDecal(ShaderTemplateMaterialDecal),
    MaterialOpaque(ShaderTemplateMaterialOpaque),
    MaterialFinalBlend(ShaderTemplateMaterialFinalBlend),
    MaterialTransparent(ShaderTemplateMaterialTransparent),
    OcclusionCull(ShaderTemplateOcclusionCull),
    OcclusionCompaction(ShaderTemplateOcclusionCompaction),
    #[cfg(feature = "lod")]
    ClusterCut(ShaderTemplateClusterCut),
    #[cfg(feature = "lod")]
    ClusterCompaction(ShaderTemplateClusterCompaction),
    Effects(ShaderTemplateEffects),
    Display(ShaderTemplateDisplay),
    Ssr(ShaderTemplateSsr),
    SsrMinzSeed(ShaderTemplateSsrMinzSeed),
    SsrMinzReduce(ShaderTemplateSsrMinzReduce),
}

impl TryFrom<&ShaderCacheKeyRenderPass> for ShaderTemplateRenderPass {
    type Error = AwsmShaderError;

    fn try_from(value: &ShaderCacheKeyRenderPass) -> std::result::Result<Self, Self::Error> {
        match value {
            ShaderCacheKeyRenderPass::Coverage(cache_key) => {
                Ok(ShaderTemplateRenderPass::Coverage(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::Geometry(cache_key) => {
                Ok(ShaderTemplateRenderPass::Geometry(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::GeometryMasked(cache_key) => Ok(
                ShaderTemplateRenderPass::GeometryMasked(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::GeometryCustomVertex(cache_key) => Ok(
                ShaderTemplateRenderPass::GeometryCustomVertex(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::GeometryMaskedCustomVertex(cache_key) => Ok(
                ShaderTemplateRenderPass::GeometryMaskedCustomVertex(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::HzbSeed(cache_key) => {
                Ok(ShaderTemplateRenderPass::HzbSeed(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::HzbReduce(cache_key) => {
                Ok(ShaderTemplateRenderPass::HzbReduce(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::BloomDownsample(cache_key) => Ok(
                ShaderTemplateRenderPass::BloomDownsample(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::BloomCombine(cache_key) => {
                Ok(ShaderTemplateRenderPass::BloomCombine(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::LightCulling(cache_key) => Ok(
                ShaderTemplateRenderPass::LightCulling(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialClassify(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialClassify(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialPrep(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialPrep(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::ShadowBlur(cache_key) => {
                Ok(ShaderTemplateRenderPass::ShadowBlur(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::DecalClassify(cache_key) => Ok(
                ShaderTemplateRenderPass::DecalClassify(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialDecal(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialDecal(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialOpaque(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialOpaque(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialFinalBlend(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialFinalBlend(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::MaterialTransparent(cache_key) => Ok(
                ShaderTemplateRenderPass::MaterialTransparent(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::OcclusionCull(cache_key) => Ok(
                ShaderTemplateRenderPass::OcclusionCull(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::OcclusionCompaction(cache_key) => Ok(
                ShaderTemplateRenderPass::OcclusionCompaction(cache_key.try_into()?),
            ),
            #[cfg(feature = "lod")]
            ShaderCacheKeyRenderPass::ClusterCut(cache_key) => {
                Ok(ShaderTemplateRenderPass::ClusterCut(cache_key.try_into()?))
            }
            #[cfg(feature = "lod")]
            ShaderCacheKeyRenderPass::ClusterCompaction(cache_key) => Ok(
                ShaderTemplateRenderPass::ClusterCompaction(cache_key.try_into()?),
            ),
            ShaderCacheKeyRenderPass::Effects(cache_key) => {
                Ok(ShaderTemplateRenderPass::Effects(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::Display(cache_key) => {
                Ok(ShaderTemplateRenderPass::Display(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::Ssr(cache_key) => {
                Ok(ShaderTemplateRenderPass::Ssr(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::SsrMinzSeed(cache_key) => {
                Ok(ShaderTemplateRenderPass::SsrMinzSeed(cache_key.try_into()?))
            }
            ShaderCacheKeyRenderPass::SsrMinzReduce(cache_key) => Ok(
                ShaderTemplateRenderPass::SsrMinzReduce(cache_key.try_into()?),
            ),
        }
    }
}

impl ShaderTemplateRenderPass {
    /// Renders the template into WGSL source.
    pub fn into_source(self) -> std::result::Result<String, AwsmShaderError> {
        match self {
            ShaderTemplateRenderPass::Coverage(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::Geometry(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::GeometryMasked(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::GeometryCustomVertex(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::GeometryMaskedCustomVertex(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::HzbSeed(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::HzbReduce(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::BloomDownsample(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::BloomCombine(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::LightCulling(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialClassify(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialPrep(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::ShadowBlur(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::DecalClassify(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialDecal(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialOpaque(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialFinalBlend(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::MaterialTransparent(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::OcclusionCull(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::OcclusionCompaction(tmpl) => tmpl.into_source(),
            #[cfg(feature = "lod")]
            ShaderTemplateRenderPass::ClusterCut(tmpl) => tmpl.into_source(),
            #[cfg(feature = "lod")]
            ShaderTemplateRenderPass::ClusterCompaction(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::Effects(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::Display(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::Ssr(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::SsrMinzSeed(tmpl) => tmpl.into_source(),
            ShaderTemplateRenderPass::SsrMinzReduce(tmpl) => tmpl.into_source(),
        }
    }

    /// Returns an optional debug label for shader compilation.
    /// Kept in release builds (see `ShaderTemplate::into_descriptor`
    /// for the cost rationale).
    pub fn debug_label(&self) -> Option<&str> {
        match self {
            ShaderTemplateRenderPass::Coverage(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::Geometry(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::GeometryMasked(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::GeometryCustomVertex(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::GeometryMaskedCustomVertex(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::HzbSeed(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::HzbReduce(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::BloomDownsample(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::BloomCombine(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::LightCulling(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialClassify(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialPrep(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::ShadowBlur(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::DecalClassify(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialDecal(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialOpaque(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialFinalBlend(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::MaterialTransparent(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::OcclusionCull(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::OcclusionCompaction(tmpl) => tmpl.debug_label(),
            #[cfg(feature = "lod")]
            ShaderTemplateRenderPass::ClusterCut(tmpl) => tmpl.debug_label(),
            #[cfg(feature = "lod")]
            ShaderTemplateRenderPass::ClusterCompaction(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::Effects(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::Display(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::Ssr(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::SsrMinzSeed(tmpl) => tmpl.debug_label(),
            ShaderTemplateRenderPass::SsrMinzReduce(tmpl) => tmpl.debug_label(),
        }
    }
}
