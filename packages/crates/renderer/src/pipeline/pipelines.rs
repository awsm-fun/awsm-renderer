//! Legacy pipeline cache for renderer pipelines.

use std::collections::HashMap;

use awsm_renderer_core::error::AwsmCoreError;
use slotmap::{new_key_type, SlotMap};
use thiserror::Error;

use crate::{bind_group::AwsmBindGroupError, shaders::ShaderKey, AwsmRenderer};

use super::{cache::PipelineLayoutCacheKey, RenderPipelineCacheKey};

/// Pipeline caches for render pipelines and layouts.
pub struct Pipelines {
    render_pipeline: SlotMap<RenderPipelineKey, web_sys::GpuRenderPipeline>,
    layout: SlotMap<PipelineLayoutKey, web_sys::GpuPipelineLayout>,
    render_pipeline_cache: HashMap<RenderPipelineCacheKey, RenderPipelineKey>,
    reverse_render_pipeline_cache: HashMap<RenderPipelineKey, RenderPipelineCacheKey>,
    layout_cache: HashMap<PipelineLayoutCacheKey, PipelineLayoutKey>,
}

impl Default for Pipelines {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipelines {
    /// Creates empty pipeline caches.
    pub fn new() -> Self {
        Self {
            render_pipeline: SlotMap::with_key(),
            layout: SlotMap::with_key(),
            render_pipeline_cache: HashMap::new(),
            reverse_render_pipeline_cache: HashMap::new(),
            layout_cache: HashMap::new(),
        }
    }

    /// Returns a pipeline layout by key.
    pub fn get_pipeline_layout(
        &self,
        key: PipelineLayoutKey,
    ) -> Result<&web_sys::GpuPipelineLayout> {
        self.layout
            .get(key)
            .ok_or(AwsmPipelineError::MissingPipelineLayout(key))
    }
    /// Returns a render pipeline by key.
    pub fn get_render_pipeline(
        &self,
        key: RenderPipelineKey,
    ) -> Result<&web_sys::GpuRenderPipeline> {
        self.render_pipeline
            .get(key)
            .ok_or(AwsmPipelineError::MissingRenderPipeline(key))
    }

    /// Looks up a pipeline layout key in the cache.
    pub fn get_pipeline_layout_key_from_cache(
        &self,
        key: &PipelineLayoutCacheKey,
    ) -> Option<PipelineLayoutKey> {
        self.layout_cache.get(key).cloned()
    }

    /// Looks up a render pipeline key in the cache.
    pub fn get_render_pipeline_key_from_cache(
        &self,
        key: &RenderPipelineCacheKey,
    ) -> Option<RenderPipelineKey> {
        self.render_pipeline_cache.get(key).cloned()
    }

    /// Returns the cache key for a render pipeline key.
    pub fn get_render_pipeline_cache_from_key(
        &self,
        key: &RenderPipelineKey,
    ) -> Option<RenderPipelineCacheKey> {
        self.reverse_render_pipeline_cache.get(key).cloned()
    }
}

impl AwsmRenderer {
    /// Adds a render pipeline to the cache.
    pub async fn add_render_pipeline(
        &mut self,
        label: Option<&str>,
        cache_key: RenderPipelineCacheKey,
    ) -> Result<RenderPipelineKey> {
        if let Some(pipeline_key) = self
            .pipelines
            .get_render_pipeline_key_from_cache(&cache_key)
        {
            return Ok(pipeline_key);
        }

        let layout = self.pipelines.layout.get(cache_key.layout_key).ok_or(
            AwsmPipelineError::MissingPipelineLayout(cache_key.layout_key),
        )?;

        let shader = self
            .shaders
            .get_shader(cache_key.shader_key)
            .ok_or(AwsmPipelineError::MissingShader(cache_key.shader_key))?;

        let pipeline = self
            .gpu
            .create_render_pipeline(&cache_key.clone().into_descriptor(shader, layout, label)?)
            .await?;

        let pipeline_key = self.pipelines.render_pipeline.insert(pipeline);

        self.pipelines
            .render_pipeline_cache
            .insert(cache_key.clone(), pipeline_key);
        self.pipelines
            .reverse_render_pipeline_cache
            .insert(pipeline_key, cache_key);

        Ok(pipeline_key)
    }
}

new_key_type! {
    /// Opaque key for render pipelines.
    pub struct RenderPipelineKey;
}

new_key_type! {
    /// Opaque key for pipeline layouts.
    pub struct PipelineLayoutKey;
}

/// Result type for pipeline operations.
type Result<T> = std::result::Result<T, AwsmPipelineError>;

/// Pipeline-related errors.
#[derive(Error, Debug)]
pub enum AwsmPipelineError {
    #[error("[render pipeline] missing pipeline: {0:?}")]
    MissingRenderPipeline(RenderPipelineKey),

    #[error("[render pipeline] missing shader: {0:?}")]
    MissingShader(ShaderKey),

    #[error("[render pipeline] missing layout: {0:?}")]
    MissingPipelineLayout(PipelineLayoutKey),

    #[error("[render pipeline] bind group: {0:?}")]
    BindGroup(#[from] AwsmBindGroupError),

    #[error("[render pipeline]: {0:?}")]
    Core(#[from] AwsmCoreError),
}
