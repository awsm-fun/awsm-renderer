//! Render pass orchestration and initialization.

pub mod coverage;
pub mod display;
pub mod effects;
pub mod geometry;
pub mod hzb;
pub mod light_culling;
pub mod lines;
pub mod material_classify;
pub mod material_decal;
pub mod material_opaque;
pub mod material_transparent;
pub mod occlusion;
pub mod shader_cache_key;
pub mod shader_template;
pub mod shared;

use awsm_renderer_core::renderer::AwsmRendererWebGpu;

use crate::error::Result;
use crate::features::RendererFeatures;
use crate::render_passes::effects::render_pass::EffectsRenderPass;
use crate::{
    bind_group_layout::BindGroupLayouts,
    pipeline_layouts::PipelineLayouts,
    pipelines::Pipelines,
    render_passes::{
        coverage::render_pass::CoverageRenderPass, display::render_pass::DisplayRenderPass,
        geometry::render_pass::GeometryRenderPass, hzb::render_pass::HzbRenderPass,
        light_culling::render_pass::LightCullingRenderPass,
        material_classify::render_pass::MaterialClassifyRenderPass,
        material_decal::render_pass::MaterialDecalRenderPass,
        material_opaque::render_pass::MaterialOpaqueRenderPass,
        material_transparent::render_pass::MaterialTransparentRenderPass,
        occlusion::compaction::CompactionRenderPass, occlusion::render_pass::OcclusionRenderPass,
    },
    render_textures::RenderTextureFormats,
    shaders::Shaders,
    textures::Textures,
};

/// Collection of render passes used by the renderer.
pub struct RenderPasses {
    pub geometry: GeometryRenderPass,
    /// GPU mesh-pixel-coverage producer. `None` when
    /// `features.coverage_lod == false`. Consumers read the resulting
    /// `MeshCoverage` table via `is_below_threshold`; with the
    /// producer disabled that always returns `false`, which routes
    /// every consumer to its "above threshold / use the expensive
    /// variant" path — the safe default.
    pub coverage: Option<CoverageRenderPass>,
    /// HZB build pass. `None` when `features.gpu_culling == false`.
    pub hzb: Option<HzbRenderPass>,
    /// GPU occlusion-cull pass. `None` when
    /// `features.gpu_culling == false`.
    pub occlusion: Option<OcclusionRenderPass>,
    /// Compaction `IndirectDrawArgs` pass. `None` when
    /// `features.gpu_culling == false`.
    pub occlusion_compaction: Option<CompactionRenderPass>,
    pub light_culling: LightCullingRenderPass,
    pub material_classify: MaterialClassifyRenderPass,
    /// Decal classify + shading + composite pass. `None` when
    /// `features.decals == false`.
    pub material_decal: Option<MaterialDecalRenderPass>,
    pub material_opaque: MaterialOpaqueRenderPass,
    pub material_transparent: MaterialTransparentRenderPass,
    pub effects: EffectsRenderPass,
    pub display: DisplayRenderPass,
}

impl RenderPasses {
    /// Creates all render passes for the renderer. Passes gated by
    /// [`RendererFeatures`] are skipped at construction; their slots
    /// stay `None`.
    pub async fn new<'a>(
        ctx: &mut RenderPassInitContext<'a>,
        features: &RendererFeatures,
    ) -> Result<Self> {
        Ok(Self {
            geometry: GeometryRenderPass::new(ctx).await?,
            coverage: if features.coverage_lod {
                Some(CoverageRenderPass::new(ctx).await?)
            } else {
                None
            },
            hzb: if features.gpu_culling {
                Some(HzbRenderPass::new(ctx).await?)
            } else {
                None
            },
            occlusion: if features.gpu_culling {
                Some(OcclusionRenderPass::new(ctx).await?)
            } else {
                None
            },
            occlusion_compaction: if features.gpu_culling {
                Some(CompactionRenderPass::new(ctx).await?)
            } else {
                None
            },
            light_culling: LightCullingRenderPass::new(ctx).await?,
            material_classify: MaterialClassifyRenderPass::new(ctx).await?,
            material_decal: if features.decals {
                Some(MaterialDecalRenderPass::new(ctx).await?)
            } else {
                None
            },
            material_opaque: MaterialOpaqueRenderPass::new(ctx).await?,
            material_transparent: MaterialTransparentRenderPass::new(ctx).await?,
            effects: EffectsRenderPass::new(ctx).await?,
            display: DisplayRenderPass::new(ctx).await?,
        })
    }
}

/// Shared context used to initialize render passes.
pub struct RenderPassInitContext<'a> {
    pub gpu: &'a mut AwsmRendererWebGpu,
    pub bind_group_layouts: &'a mut BindGroupLayouts,
    pub textures: &'a mut Textures,
    pub pipeline_layouts: &'a mut PipelineLayouts,
    pub pipelines: &'a mut Pipelines,
    pub shaders: &'a mut Shaders,
    pub render_texture_formats: &'a mut RenderTextureFormats,
    /// Active feature gates. Lets construction-time code (e.g. the
    /// decal classify pass's HZB binding switch) pick the variant
    /// that matches the live feature set.
    pub features: &'a RendererFeatures,
}
