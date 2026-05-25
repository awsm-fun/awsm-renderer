//! Compute pipeline cache.

use std::collections::{BTreeMap, HashMap};

use awsm_renderer_core::{
    error::AwsmCoreError,
    pipeline::{
        constants::{ConstantOverrideKey, ConstantOverrideValue},
        layout::PipelineLayoutKind,
        ComputePipelineDescriptor, ProgrammableStage,
    },
    renderer::AwsmRendererWebGpu,
};
use slotmap::{new_key_type, SlotMap};
use thiserror::Error;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::{
    bind_groups::AwsmBindGroupError,
    pipeline_layouts::{AwsmPipelineLayoutError, PipelineLayoutKey, PipelineLayouts},
    shaders::{ShaderKey, Shaders},
};

/// Cache of compute pipelines by key.
pub struct ComputePipelines {
    lookup: SlotMap<ComputePipelineKey, web_sys::GpuComputePipeline>,
    cache: HashMap<ComputePipelineCacheKey, ComputePipelineKey>,
}

impl ComputePipelines {
    /// Creates an empty compute pipeline cache.
    pub fn new() -> Self {
        Self {
            lookup: SlotMap::with_key(),
            cache: HashMap::new(),
        }
    }

    /// Returns a pipeline key, creating the pipeline if needed.
    ///
    /// Thin wrapper over [`Self::ensure_keys`] — funnelling the
    /// single-key path through the same code keeps the cache-hit
    /// fast path and the cache-miss creation path in one place.
    pub async fn get_key(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        shaders: &Shaders,
        pipeline_layouts: &PipelineLayouts,
        cache_key: ComputePipelineCacheKey,
    ) -> Result<ComputePipelineKey> {
        // Fast path: cache hit, no allocation, no async.
        if let Some(key) = self.cache.get(&cache_key) {
            return Ok(*key);
        }
        let keys = self
            .ensure_keys(gpu, shaders, pipeline_layouts, std::iter::once(cache_key))
            .await?;
        Ok(keys[0])
    }

    /// Pre-warms the cache for a batch of compute pipeline keys,
    /// issuing every `createComputePipelineAsync` call back-to-back
    /// before awaiting any of them. See
    /// [`crate::pipelines::render_pipeline::RenderPipelines::ensure_keys`]
    /// for the rationale.
    pub async fn ensure_keys<I>(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        shaders: &Shaders,
        pipeline_layouts: &PipelineLayouts,
        cache_keys: I,
    ) -> Result<Vec<ComputePipelineKey>>
    where
        I: IntoIterator<Item = ComputePipelineCacheKey>,
    {
        // Each cache key is allocated/cloned exactly once — when it
        // crosses the IntoIterator boundary into `inputs`. Cache misses
        // keep the key alive in `inputs` until install time, then move
        // it into `self.cache` via `Option::take`. See
        // `RenderPipelines::ensure_keys` for the longer rationale.
        let inputs: Vec<ComputePipelineCacheKey> = cache_keys.into_iter().collect();
        let mut slot: Vec<Option<ComputePipelineKey>> = vec![None; inputs.len()];

        let mut pending_input_indices: Vec<usize> = Vec::new();
        let mut pending_targets: Vec<Vec<usize>> = Vec::new();

        {
            let mut seen: HashMap<&ComputePipelineCacheKey, usize> = HashMap::new();
            for (i, cache_key) in inputs.iter().enumerate() {
                if let Some(key) = self.cache.get(cache_key) {
                    slot[i] = Some(*key);
                    continue;
                }
                if let Some(&pending_idx) = seen.get(cache_key) {
                    pending_targets[pending_idx].push(i);
                    continue;
                }
                seen.insert(cache_key, pending_input_indices.len());
                pending_targets.push(vec![i]);
                pending_input_indices.push(i);
            }
        }

        if pending_input_indices.is_empty() {
            return Ok(slot.into_iter().map(Option::unwrap).collect());
        }

        let mut descriptors: Vec<web_sys::GpuComputePipelineDescriptor> =
            Vec::with_capacity(pending_input_indices.len());
        for &input_idx in &pending_input_indices {
            descriptors.push(build_descriptor(
                &inputs[input_idx],
                shaders,
                pipeline_layouts,
            )?);
        }

        let promises: Vec<JsFuture<web_sys::GpuComputePipeline>> = descriptors
            .iter()
            .map(|d| JsFuture::from(gpu.create_compute_pipeline_promise(d)))
            .collect();

        let results = futures::future::join_all(promises).await;

        let mut inputs_owned: Vec<Option<ComputePipelineCacheKey>> =
            inputs.into_iter().map(Some).collect();

        for ((input_idx, result), input_targets) in pending_input_indices
            .into_iter()
            .zip(results)
            .zip(pending_targets)
        {
            let pipeline: web_sys::GpuComputePipeline = result
                .map_err(|e| AwsmComputePipelineError::Core(AwsmCoreError::pipeline_creation(e)))?
                .unchecked_into();
            let key = self.lookup.insert(pipeline);
            let cache_key = inputs_owned[input_idx]
                .take()
                .expect("pending input slot must own its key");
            self.cache.insert(cache_key, key);
            for i in input_targets {
                slot[i] = Some(key);
            }
        }

        Ok(slot.into_iter().map(Option::unwrap).collect())
    }

    /// Returns a compute pipeline for a key.
    pub fn get(&self, key: ComputePipelineKey) -> Result<&web_sys::GpuComputePipeline> {
        self.lookup
            .get(key)
            .ok_or(AwsmComputePipelineError::NotFound(key))
    }
}

/// Builds a `GpuComputePipelineDescriptor` from a cache key.
fn build_descriptor(
    cache_key: &ComputePipelineCacheKey,
    shaders: &Shaders,
    pipeline_layouts: &PipelineLayouts,
) -> Result<web_sys::GpuComputePipelineDescriptor> {
    let shader_module =
        shaders
            .get(cache_key.shader_key)
            .ok_or(AwsmComputePipelineError::MissingShader(
                cache_key.shader_key,
            ))?;

    let layout = pipeline_layouts.get(cache_key.layout_key)?;

    let mut programmable_stage = ProgrammableStage::new(shader_module, None);
    programmable_stage.constant_overrides = cache_key.constant_overrides.clone();

    let descriptor = ComputePipelineDescriptor::new(
        programmable_stage,
        PipelineLayoutKind::Custom(layout),
        None,
    );

    Ok(descriptor.into())
}

impl Default for ComputePipelines {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache key for compute pipeline creation.
#[derive(Hash, Debug, Clone, PartialEq, Eq)]
pub struct ComputePipelineCacheKey {
    pub shader_key: ShaderKey,
    pub layout_key: PipelineLayoutKey,
    pub constant_overrides: BTreeMap<ConstantOverrideKey, ConstantOverrideValue>,
}

impl ComputePipelineCacheKey {
    /// Creates a cache key with shader and layout keys.
    pub fn new(shader_key: ShaderKey, layout_key: PipelineLayoutKey) -> Self {
        Self {
            shader_key,
            layout_key,
            constant_overrides: BTreeMap::new(),
        }
    }

    /// Adds a constant override to the cache key.
    pub fn with_push_constant_override(
        mut self,
        key: ConstantOverrideKey,
        value: ConstantOverrideValue,
    ) -> Self {
        self.constant_overrides.insert(key, value);
        self
    }
}

new_key_type! {
    /// Opaque key for compute pipelines.
    pub struct ComputePipelineKey;
}

/// Result type for compute pipeline operations.
type Result<T> = std::result::Result<T, AwsmComputePipelineError>;

/// Compute pipeline errors.
#[derive(Error, Debug)]
pub enum AwsmComputePipelineError {
    #[error("[compute pipeline] missing pipeline: {0:?}")]
    NotFound(ComputePipelineKey),

    #[error("[compute pipeline] missing shader: {0:?}")]
    MissingShader(ShaderKey),

    #[error("[compute pipeline] bind group: {0:?}")]
    BindGroup(#[from] AwsmBindGroupError),

    #[error("[compute pipeline]: {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[compute pipeline] {0:?}")]
    Layout(#[from] AwsmPipelineLayoutError),
}
