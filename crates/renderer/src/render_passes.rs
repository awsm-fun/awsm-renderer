//! Render pass orchestration and initialization.

pub mod display;
pub mod effects;
pub mod geometry;
pub mod light_culling;
pub mod lines;
pub mod material_classify;
pub mod material_opaque;
pub mod material_transparent;
pub mod shader_cache_key;
pub mod shader_template;
pub mod shared;

use awsm_renderer_core::renderer::AwsmRendererWebGpu;

use crate::error::Result;
use crate::render_passes::effects::render_pass::EffectsRenderPass;
use crate::{
    bind_group_layout::BindGroupLayouts,
    pipeline_layouts::PipelineLayouts,
    pipelines::Pipelines,
    render_passes::{
        display::render_pass::DisplayRenderPass, geometry::render_pass::GeometryRenderPass,
        light_culling::render_pass::LightCullingRenderPass,
        material_classify::render_pass::MaterialClassifyRenderPass,
        material_opaque::render_pass::MaterialOpaqueRenderPass,
        material_transparent::render_pass::MaterialTransparentRenderPass,
    },
    render_textures::RenderTextureFormats,
    shaders::Shaders,
    textures::Textures,
};

/// Collection of render passes used by the renderer.
pub struct RenderPasses {
    pub geometry: GeometryRenderPass,
    pub light_culling: LightCullingRenderPass,
    pub material_classify: MaterialClassifyRenderPass,
    pub material_opaque: MaterialOpaqueRenderPass,
    pub material_transparent: MaterialTransparentRenderPass,
    pub effects: EffectsRenderPass,
    pub display: DisplayRenderPass,
}

impl RenderPasses {
    /// Creates all render passes for the renderer.
    pub async fn new<'a>(ctx: &mut RenderPassInitContext<'a>) -> Result<Self> {
        Ok(Self {
            geometry: GeometryRenderPass::new(ctx).await?,
            light_culling: LightCullingRenderPass::new(ctx).await?,
            material_classify: MaterialClassifyRenderPass::new(ctx).await?,
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
}
