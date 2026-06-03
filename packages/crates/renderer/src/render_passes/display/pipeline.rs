//! Display pass pipeline setup.

use awsm_renderer_core::{
    pipeline::{fragment::ColorTargetState, primitive::PrimitiveState},
    renderer::AwsmRendererWebGpu,
};

use crate::{
    error::Result,
    pipeline_layouts::{PipelineLayoutCacheKey, PipelineLayoutKey, PipelineLayouts},
    pipelines::{
        render_pipeline::{RenderPipelineCacheKey, RenderPipelineKey},
        Pipelines,
    },
    post_process::PostProcessing,
    render_passes::{
        display::{bind_group::DisplayBindGroups, shader::cache_key::ShaderCacheKeyDisplay},
        RenderPassInitContext,
    },
    render_textures::RenderTextureFormats,
    shaders::{ShaderCacheKey, Shaders},
};

/// Pipeline layout and render pipeline for the display pass.
pub struct DisplayPipelines {
    pub pipeline_layout_key: PipelineLayoutKey,
    pub render_pipeline_key: Option<RenderPipelineKey>,
}

/// Pre-resolved shader + render-pipeline cache key for the display
/// pass. Returned by [`DisplayPipelines::build_descriptors`] and
/// consumed by [`DisplayPipelines::install_resolved`].
pub struct DisplayPipelinesDescriptors {
    pub shader_cache_keys: Vec<ShaderCacheKey>,
    pub pipeline_cache_keys: Vec<RenderPipelineCacheKey>,
}

impl DisplayPipelines {
    /// Creates pipeline layout state for the display pass.
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &DisplayBindGroups,
    ) -> Result<Self> {
        let pipeline_layout_cache_key =
            PipelineLayoutCacheKey::new(vec![bind_groups.bind_group_layout_key]);

        let pipeline_layout_key = ctx.pipeline_layouts.get_key(
            ctx.gpu,
            ctx.bind_group_layouts,
            pipeline_layout_cache_key,
        )?;

        Ok(Self {
            pipeline_layout_key,
            render_pipeline_key: None,
        })
    }

    /// Returns the shader cache key for the current PP config. Folded
    /// into the cross-tail `Shaders::ensure_keys` batch.
    pub fn shader_cache_keys_for(post_processing: &PostProcessing) -> Vec<ShaderCacheKey> {
        vec![ShaderCacheKey::from(ShaderCacheKeyDisplay {
            tonemapping: post_processing.tonemapping,
        })]
    }

    /// Builds the (shader-cache-key, render-pipeline-cache-key) pair.
    /// The shader must already be warm in the cache.
    pub async fn build_descriptors(
        &self,
        post_processing: &PostProcessing,
        gpu: &AwsmRendererWebGpu,
        shaders: &mut Shaders,
    ) -> Result<DisplayPipelinesDescriptors> {
        let shader_cache_key = ShaderCacheKeyDisplay {
            tonemapping: post_processing.tonemapping,
        };
        let shader_key = shaders.get_key(gpu, shader_cache_key.clone()).await?;
        let render_pipeline_cache_key =
            RenderPipelineCacheKey::new(shader_key, self.pipeline_layout_key)
                .with_push_fragment_target(ColorTargetState::new(gpu.current_context_format()))
                .with_primitive(
                    PrimitiveState::new()
                        .with_topology(web_sys::GpuPrimitiveTopology::TriangleList)
                        .with_cull_mode(web_sys::GpuCullMode::None)
                        .with_front_face(web_sys::GpuFrontFace::Ccw),
                );
        Ok(DisplayPipelinesDescriptors {
            shader_cache_keys: vec![ShaderCacheKey::from(shader_cache_key)],
            pipeline_cache_keys: vec![render_pipeline_cache_key],
        })
    }

    /// Writes the resolved render pipeline key.
    pub fn install_resolved(&mut self, resolved: Vec<RenderPipelineKey>) {
        debug_assert_eq!(resolved.len(), 1);
        self.render_pipeline_key = Some(resolved[0]);
    }

    /// Updates the render pipeline for the current post-processing
    /// settings. Used by [`crate::AwsmRenderer::set_post_processing`]
    /// for mid-session config changes — at startup the orchestrator
    /// goes through [`Self::build_descriptors`] +
    /// [`Self::install_resolved`] directly.
    pub async fn set_render_pipeline_key(
        &mut self,
        post_processing: &PostProcessing,
        gpu: &AwsmRendererWebGpu,
        shaders: &mut Shaders,
        pipelines: &mut Pipelines,
        pipeline_layouts: &PipelineLayouts,
        _render_texture_formats: &RenderTextureFormats,
    ) -> Result<()> {
        let shader_cache_keys = Self::shader_cache_keys_for(post_processing);
        shaders
            .ensure_keys(gpu, shader_cache_keys.iter().cloned())
            .await?;
        let descs = self
            .build_descriptors(post_processing, gpu, shaders)
            .await?;
        let resolved = pipelines
            .render
            .ensure_keys(gpu, shaders, pipeline_layouts, descs.pipeline_cache_keys)
            .await?;
        self.install_resolved(resolved);
        Ok(())
    }
}
