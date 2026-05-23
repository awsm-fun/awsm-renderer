//! Bind group layout caching.

use std::collections::HashMap;

use awsm_renderer_core::{
    bind_groups::{BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindGroupLayoutResource},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};
use slotmap::{new_key_type, SlotMap};
use thiserror::Error;

/// Cache for GPU bind group layouts.
pub struct BindGroupLayouts {
    lookup: SlotMap<BindGroupLayoutKey, web_sys::GpuBindGroupLayout>,
    cache: HashMap<BindGroupLayoutCacheKey, BindGroupLayoutKey>,
    #[cfg(debug_assertions)]
    pub max: BindGroupLayoutCounter,
}

impl BindGroupLayouts {
    /// Creates an empty bind group layout cache.
    pub fn new() -> Self {
        Self {
            lookup: SlotMap::with_key(),
            cache: HashMap::new(),
            #[cfg(debug_assertions)]
            max: BindGroupLayoutCounter::default(),
        }
    }

    /// Returns a layout key for the cache key, creating it if needed.
    pub fn get_key(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        cache_key: BindGroupLayoutCacheKey,
    ) -> Result<BindGroupLayoutKey> {
        if let Some(key) = self.cache.get(&cache_key) {
            return Ok(*key);
        }

        #[cfg(debug_assertions)]
        self.update_max_counter(&cache_key);

        let entries = cache_key
            .entries
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, entry)| BindGroupLayoutEntry {
                binding: index as u32,
                visibility_compute: entry.visibility_compute,
                visibility_vertex: entry.visibility_vertex,
                visibility_fragment: entry.visibility_fragment,
                resource: entry.resource,
            })
            .collect();

        let bind_group_layout = gpu
            .create_bind_group_layout(
                &BindGroupLayoutDescriptor::new(None)
                    .with_entries(entries)
                    .into(),
            )
            .map_err(AwsmBindGroupLayoutError::Create)?;

        let key = self.lookup.insert(bind_group_layout);
        self.cache.insert(cache_key, key);
        Ok(key)
    }

    /// Returns the layout for a given key.
    pub fn get(&self, key: BindGroupLayoutKey) -> Result<&web_sys::GpuBindGroupLayout> {
        self.lookup
            .get(key)
            .ok_or(AwsmBindGroupLayoutError::NotFound(key))
    }

    #[cfg(debug_assertions)]
    fn update_max_counter(&mut self, cache_key: &BindGroupLayoutCacheKey) {
        use crate::COMPATIBITLIY_REQUIREMENTS;
        use awsm_renderer_core::bind_groups::BufferBindingType;

        let mut counter = BindGroupLayoutCounter::default();
        // Counts ONLY storage buffers — the
        // `maxStorageBuffersPerShaderStage` adapter limit (which the
        // compatibility requirement mirrors) doesn't count uniform-
        // buffer bindings. Mixing them would produce spurious warnings
        // every time we add a uniform binding while still being well
        // under the storage cap.
        let mut storage_buffers: u32 = 0;

        for entry in &cache_key.entries {
            match &entry.resource {
                BindGroupLayoutResource::Buffer(layout) => {
                    counter.buffers += 1;
                    let is_storage = matches!(
                        layout.binding_type,
                        Some(BufferBindingType::Storage) | Some(BufferBindingType::ReadOnlyStorage)
                    );
                    if is_storage {
                        storage_buffers += 1;
                    }
                }
                BindGroupLayoutResource::Sampler { .. } => {
                    counter.samplers += 1;
                }
                BindGroupLayoutResource::Texture { .. } => {
                    counter.textures += 1;
                }
                BindGroupLayoutResource::StorageTexture { .. } => {
                    counter.storage_textures += 1;
                }
                BindGroupLayoutResource::ExternalTexture => {
                    counter.external_textures += 1;
                }
            }
        }

        let before = self.max.clone();

        self.max.buffers = self.max.buffers.max(counter.buffers);
        self.max.samplers = self.max.samplers.max(counter.samplers);
        self.max.textures = self.max.textures.max(counter.textures);
        self.max.storage_textures = self.max.storage_textures.max(counter.storage_textures);
        self.max.external_textures = self.max.external_textures.max(counter.external_textures);
        self.max.storage_buffers = self.max.storage_buffers.max(storage_buffers);

        if before != self.max {
            tracing::debug!("Updated BindGroupLayout max counts: {:#?}", self.max);
        }

        if let Some(required) = COMPATIBITLIY_REQUIREMENTS.storage_buffers {
            if self.max.storage_buffers > required {
                tracing::warn!(
                    "Max bind-group-layout STORAGE buffers {} exceeds compatibility requirement {}",
                    self.max.storage_buffers,
                    required
                );
            }
        }
    }
}

#[cfg(debug_assertions)]
#[derive(Default, Debug, Clone, PartialEq, Eq)]
/// Debug counter for bind group layout limits.
pub struct BindGroupLayoutCounter {
    /// Total buffer bindings (uniform + storage). Reported for parity
    /// with the legacy debug output.
    pub buffers: u32,
    /// Storage buffer bindings only — the
    /// `maxStorageBuffersPerShaderStage` limit and the compatibility
    /// requirement both count this, not the total.
    pub storage_buffers: u32,
    pub samplers: u32,
    pub textures: u32,
    pub storage_textures: u32,
    pub external_textures: u32,
}

#[cfg(debug_assertions)]
impl Default for BindGroupLayouts {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Hash, Debug, Clone, PartialEq, Eq)]
/// Cache key for bind group layouts.
pub struct BindGroupLayoutCacheKey {
    pub entries: Vec<BindGroupLayoutCacheKeyEntry>,
}
impl BindGroupLayoutCacheKey {
    /// Creates a cache key from entries.
    pub fn new(entries: Vec<BindGroupLayoutCacheKeyEntry>) -> Self {
        Self { entries }
    }
}

#[derive(Hash, Debug, Clone, PartialEq, Eq)]
/// Single entry in a bind group layout cache key.
pub struct BindGroupLayoutCacheKeyEntry {
    pub resource: BindGroupLayoutResource,
    pub visibility_compute: bool,
    pub visibility_vertex: bool,
    pub visibility_fragment: bool,
}

new_key_type! {
    /// Opaque key for cached bind group layouts.
    pub struct BindGroupLayoutKey;
}

/// Result type for bind group layout operations.
type Result<T> = std::result::Result<T, AwsmBindGroupLayoutError>;
/// Bind group layout errors.
#[derive(Error, Debug)]
pub enum AwsmBindGroupLayoutError {
    #[error("[bind group layout] Unable to create: {0:?}")]
    Create(AwsmCoreError),

    #[error("[bind group layout] Not found: {0:?}")]
    NotFound(BindGroupLayoutKey),
}
