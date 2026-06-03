//! Pipeline layout caching.

use std::collections::HashMap;

use awsm_renderer_core::{
    error::AwsmCoreError, pipeline::layout::PipelineLayoutDescriptor, renderer::AwsmRendererWebGpu,
};
use slotmap::{new_key_type, SlotMap};
use thiserror::Error;

use crate::bind_group_layout::{AwsmBindGroupLayoutError, BindGroupLayoutKey, BindGroupLayouts};

/// Cache for GPU pipeline layouts.
pub struct PipelineLayouts {
    lookup: SlotMap<PipelineLayoutKey, web_sys::GpuPipelineLayout>,
    cache: HashMap<PipelineLayoutCacheKey, PipelineLayoutKey>,
}

impl PipelineLayouts {
    /// Creates an empty pipeline layout cache.
    pub fn new() -> Self {
        Self {
            lookup: SlotMap::with_key(),
            cache: HashMap::new(),
        }
    }

    /// Returns a layout key for the cache key, creating it if needed.
    pub fn get_key(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        bind_group_layouts: &BindGroupLayouts,
        cache_key: PipelineLayoutCacheKey,
    ) -> Result<PipelineLayoutKey> {
        if let Some(key) = self.cache.get(&cache_key) {
            return Ok(*key);
        }

        let pipeline_bind_group_layouts = cache_key
            .bind_group_layouts
            .iter()
            .map(|key| {
                bind_group_layouts
                    .get(*key)
                    .cloned()
                    .map_err(AwsmPipelineLayoutError::BindGroupLayout)
            })
            .collect::<Result<Vec<_>>>()?;

        let pipeline_layout = gpu.create_pipeline_layout(
            &PipelineLayoutDescriptor::new(None, pipeline_bind_group_layouts).into(),
        );

        let key = self.lookup.insert(pipeline_layout);
        self.cache.insert(cache_key, key);
        Ok(key)
    }

    /// Returns the layout for a given key.
    pub fn get(&self, key: PipelineLayoutKey) -> Result<&web_sys::GpuPipelineLayout> {
        self.lookup
            .get(key)
            .ok_or(AwsmPipelineLayoutError::NotFound(key))
    }
}

impl Default for PipelineLayouts {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache key for pipeline layouts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PipelineLayoutCacheKey {
    pub bind_group_layouts: Vec<BindGroupLayoutKey>,
}

impl PipelineLayoutCacheKey {
    /// Creates a cache key from bind group layout keys.
    pub fn new(bind_group_layouts: Vec<BindGroupLayoutKey>) -> Self {
        Self { bind_group_layouts }
    }
}

new_key_type! {
    /// Opaque key for pipeline layouts.
    pub struct PipelineLayoutKey;
}

/// Result type for pipeline layout operations.
type Result<T> = std::result::Result<T, AwsmPipelineLayoutError>;

/// Pipeline layout errors.
#[derive(Error, Debug)]
pub enum AwsmPipelineLayoutError {
    #[error("[pipeline layout] Unable to create: {0:?}")]
    Create(AwsmCoreError),

    #[error("[pipeline layout] Not found: {0:?}")]
    NotFound(PipelineLayoutKey),

    #[error("[pipeline layout] {0:?}")]
    BindGroupLayout(#[from] AwsmBindGroupLayoutError),
}
