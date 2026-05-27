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

        let n = descriptors.len();
        let t_start = web_sys::js_sys::Date::now();

        // Per-pipeline label + cumulative-timing wrapper (Lessons A from
        // docs/plans/more-optimizations.md). Each individual future is
        // wrapped to log on resolve with its finish-order index and
        // cumulative wall-clock since batch start. **Critically:** the
        // wrapping uses an `async move { ... promise.await ... }` block
        // per promise — the `join_all` below still drives every future
        // concurrently. Do NOT replace `join_all(promises).await` with
        // a serial `for promise in promises { promise.await }` loop:
        // that defeats Dawn's parallel compile pool.
        let labels: Vec<String> = pending_input_indices
            .iter()
            .map(|&input_idx| {
                let ck = &inputs[input_idx];
                let shader_label = shaders
                    .get_label(ck.shader_key)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("{:?}", ck.shader_key));
                format!("{}:{:?}", shader_label, ck.layout_key)
            })
            .collect();
        // Per-promise finish-time recording — captures the cumulative
        // wall-clock at the moment each individual promise resolves so
        // we can emit a sort-by-finish summary at the end (E.4).
        let finish_times: std::rc::Rc<std::cell::RefCell<Vec<(usize, f64, bool)>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::with_capacity(n)));
        let promises = descriptors
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let label = labels[i].clone();
                let total = n;
                let promise = JsFuture::from(gpu.create_compute_pipeline_promise(d));
                let ft = finish_times.clone();
                async move {
                    let r = promise.await;
                    let cum_ms = web_sys::js_sys::Date::now() - t_start;
                    let ok = r.is_ok();
                    ft.borrow_mut().push((i, cum_ms, ok));
                    let outcome = if ok { "ok" } else { "ERR" };
                    tracing::info!(
                        target: "awsm_renderer::boot_timing",
                        "pipeline {}/{} compute:{} cum={:.0}ms {}",
                        i + 1,
                        total,
                        label,
                        cum_ms,
                        outcome,
                    );
                    r
                }
            })
            .collect::<Vec<_>>();

        let results = futures::future::join_all(promises).await;
        let dt_ms = web_sys::js_sys::Date::now() - t_start;
        // One log line per batched ensure_keys call.
        tracing::info!(
            target: "awsm_renderer::boot_timing",
            "ComputePipelines::ensure_keys: {n} pipelines compiled in {dt_ms:.0}ms",
        );
        // E.4: sort-by-finish-time summary — operator can scan to see
        // which compute pipeline finished last (the long pole). Only
        // emitted for batches of 2+ (single-pipeline batches don't
        // benefit). Bracket order = chronological resolution order.
        if n >= 2 {
            let mut ft = finish_times.borrow_mut();
            ft.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let summary: Vec<String> = ft
                .iter()
                .map(|(i, cum, ok)| {
                    format!("{}{}@{:.0}ms", labels[*i], if *ok { "" } else { "!" }, cum)
                })
                .collect();
            tracing::info!(
                target: "awsm_renderer::boot_timing",
                "  finish-order [compute, {n} pipes]: {}",
                summary.join(" → "),
            );
        }

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

    // Debug label — same rationale as the render pipeline cache:
    // `compute:<shader>:<layout>` makes WebGPU dev-tool / validation
    // errors trivially attributable. The label string lives only
    // until `descriptor.into()` copies it into the JS-side descriptor.
    let label = format!(
        "compute:{:?}:{:?}",
        cache_key.shader_key, cache_key.layout_key
    );
    let descriptor = ComputePipelineDescriptor::new(
        programmable_stage,
        PipelineLayoutKind::Custom(layout),
        Some(&label),
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
