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
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

use std::future::Future;
use std::pin::Pin;

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

/// Sync-side prep state captured by [`ComputePipelines::ensure_keys_prepare`]
/// and consumed by [`ComputePipelines::ensure_keys_install`]. Holds the
/// cache-key inputs + per-input slot indices + the labels + finish-time
/// recorder that the `.inspect`-wrapped promises feed into. **No
/// references to `ComputePipelines` are held** — the prep state owns
/// everything it needs; install reattaches to the cache via `&mut self`.
pub struct ComputePipelinesPrep {
    /// Original cache keys in input order.
    pub inputs: Vec<ComputePipelineCacheKey>,
    /// Per-input resolved slots. `None` entries get populated by
    /// `ensure_keys_install`; `Some` entries are cache hits filled by
    /// the wrapper API before `prepare` ran.
    pub slot: Vec<Option<ComputePipelineKey>>,
    /// Indices into `inputs` whose compile promise is in flight.
    /// `pending_targets[i]` lists every input slot that wants the
    /// resolved key from this pending index (handles intra-batch
    /// duplicates).
    pub pending_input_indices: Vec<usize>,
    /// Per-pending-index list of input slots wanting the resolved key.
    pub pending_targets: Vec<Vec<usize>>,
    /// Per-pending-index display labels (used in finish-order summary).
    pub labels: Vec<String>,
    /// Batch start time (ms via `Date.now()`); fed into per-promise
    /// finish-time recording + the batch summary log line.
    pub t_start: f64,
    /// Per-promise finish-time + ok-flag recorder. Filled in by the
    /// wrapped promise closures during await; sort-by-finish summary
    /// runs in `ensure_keys_install`.
    pub finish_times: std::rc::Rc<std::cell::RefCell<Vec<(usize, f64, bool)>>>,
}

/// Bundle returned by [`ComputePipelines::ensure_keys_prepare`]: the
/// prep state + the wrapped promises ready to be awaited.
///
/// The async wrapper [`ComputePipelines::ensure_keys`] takes the
/// promises via `mem::take`, awaits via `join_all`, then hands the
/// results + prep state to `ensure_keys_install`. The
/// `pipeline_scheduler` can instead push each promise into its own
/// `FuturesUnordered` to drive compiles between frames.
pub struct ComputePipelinesPrepWithPromises {
    /// Sync-side prep state — owned, passed through to install.
    pub prep: ComputePipelinesPrep,
    /// `'static` futures — each resolves to the raw
    /// `GpuComputePipeline` JsValue or a creation error. Owns its
    /// label + finish-time recorder; safe to push into a scheduler.
    pub promises: Vec<
        Pin<Box<dyn Future<Output = std::result::Result<web_sys::GpuComputePipeline, JsValue>>>>,
    >,
}

impl ComputePipelines {
    /// Creates an empty compute pipeline cache.
    pub fn new() -> Self {
        Self {
            lookup: SlotMap::with_key(),
            cache: HashMap::new(),
        }
    }

    /// Number of compiled compute pipelines (observability / leak checks —
    /// a climbing count on a stable scene means cache keys are churning).
    pub fn len(&self) -> usize {
        self.lookup.len()
    }

    /// Evict the specific pool entries named by their `ComputePipelineKey`s
    /// (the slotmap keys a typed per-pass cache was holding). Drops the slotmap
    /// entries (releasing the `GpuComputePipeline`s) AND the reverse cache rows
    /// that pointed at them, and returns the [`ShaderKey`]s those pipelines were
    /// built from so the caller can free the matching shader modules.
    ///
    /// This is the core of the dynamic-material pipeline-leak fix: the typed
    /// per-pass caches (opaque / edge / classify) used to DROP these references
    /// on a bucket-set change while the GPU pipelines lingered in this pool
    /// forever. Freeing exactly the keys a typed cache just dropped is
    /// dangle-free by construction — nothing references them anymore (the typed
    /// cache is empty and the per-frame renderables rebuild from it).
    pub fn remove_pipeline_keys(
        &mut self,
        keys: &[ComputePipelineKey],
    ) -> std::collections::HashSet<ShaderKey> {
        if keys.is_empty() {
            return std::collections::HashSet::new();
        }
        let keyset: std::collections::HashSet<ComputePipelineKey> = keys.iter().copied().collect();
        let mut shader_keys = std::collections::HashSet::new();
        self.cache.retain(|cache_key, slot| {
            if keyset.contains(slot) {
                shader_keys.insert(cache_key.shader_key);
                false
            } else {
                true
            }
        });
        for key in keys {
            self.lookup.remove(*key);
        }
        shader_keys
    }

    /// Evict every cached compute pipeline built from one of `shader_keys`.
    /// Drops the reverse cache rows and the slotmap entries (releasing the
    /// `GpuComputePipeline`s); returns how many were freed. Used by the
    /// dynamic-material pipeline-leak fix to reclaim DETACHED orphans — pool
    /// pipelines no longer referenced by any typed cache (their reservation's
    /// resolution was dropped, or the slot was replaced before a clear ran).
    /// Their only handle is the shader they were built from. The caller MUST
    /// have already cleared/pruned every typed cache that could reference these
    /// (so nothing dangles).
    pub fn remove_by_shader_keys(
        &mut self,
        shader_keys: &std::collections::HashSet<ShaderKey>,
    ) -> usize {
        let mut removed_slots = Vec::new();
        self.cache.retain(|cache_key, slot| {
            if shader_keys.contains(&cache_key.shader_key) {
                removed_slots.push(*slot);
                false
            } else {
                true
            }
        });
        for slot in &removed_slots {
            self.lookup.remove(*slot);
        }
        removed_slots.len()
    }

    /// True when no compute pipelines exist.
    pub fn is_empty(&self) -> bool {
        self.lookup.is_empty()
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
    /// before awaiting any of them.
    ///
    /// Convenience wrapper around the factored
    /// [`Self::ensure_keys_prepare`] + [`Self::ensure_keys_install`]
    /// trio (Block D.1). The factored API lets the
    /// `pipeline_scheduler` push real compile promises into
    /// `FuturesUnordered` while keeping all existing call sites — which
    /// just want a `.await` — working with one line.
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
        // Cache-hit fast path: avoid building descriptors / issuing
        // promises for any input whose key is already in the cache.
        //
        // **Within-batch miss dedup**: if the same uncached cache key
        // appears more than once in the input batch (common in
        // per-mesh transparent rebuilds + relaunch loops over
        // registered shader_ids), we issue ONE
        // `createComputePipelineAsync` promise per unique miss key
        // and fan the resolved key back to every input slot that
        // wanted it. Without this dedup, Dawn ran every duplicate
        // promise to completion and `ensure_keys_install`'s post-hoc
        // cache-hit guard discarded all but one — the compile cost
        // was already spent.
        let inputs: Vec<ComputePipelineCacheKey> = cache_keys.into_iter().collect();
        let mut slot: Vec<Option<ComputePipelineKey>> = vec![None; inputs.len()];
        let mut unique_miss_keys: Vec<ComputePipelineCacheKey> = Vec::new();
        // For each unique miss key, the input slots that should
        // receive the resolved pipeline key.
        let mut unique_miss_targets: Vec<Vec<usize>> = Vec::new();
        let mut unique_miss_index_for_key: HashMap<ComputePipelineCacheKey, usize> = HashMap::new();
        for (i, k) in inputs.iter().enumerate() {
            if let Some(key) = self.cache.get(k) {
                slot[i] = Some(*key);
            } else if let Some(&u_idx) = unique_miss_index_for_key.get(k) {
                unique_miss_targets[u_idx].push(i);
            } else {
                let u_idx = unique_miss_keys.len();
                unique_miss_keys.push(k.clone());
                unique_miss_targets.push(vec![i]);
                unique_miss_index_for_key.insert(k.clone(), u_idx);
            }
        }
        if unique_miss_keys.is_empty() {
            return Ok(slot.into_iter().map(Option::unwrap).collect());
        }
        let mut prepped =
            Self::ensure_keys_prepare(gpu, shaders, pipeline_layouts, unique_miss_keys)?;
        let promises = std::mem::take(&mut prepped.promises);
        let results = futures::future::join_all(promises).await;
        let resolved = self.ensure_keys_install(prepped.prep, results)?;
        // `resolved` has one entry per unique miss; fan out to every
        // input slot that wanted it.
        for (key, targets) in resolved.into_iter().zip(unique_miss_targets) {
            for i in targets {
                slot[i] = Some(key);
            }
        }
        Ok(slot.into_iter().map(Option::unwrap).collect())
    }

    /// Sync phase 1 of [`Self::ensure_keys`] — dedups inputs, builds
    /// descriptors, issues every `createComputePipelineAsync` Promise
    /// back-to-back. Returns the prep state + the wrapped futures
    /// (which can be `join_all`'d or pushed into a
    /// [`futures::stream::FuturesUnordered`]).
    ///
    /// **No `&mut self` needed** — slot allocation happens entirely in
    /// [`Self::ensure_keys_install`] from the awaited results. This
    /// keeps the prep step composable with the
    /// `pipeline_scheduler::PipelineScheduler` which holds its own
    /// `FuturesUnordered` and wants `'static` futures.
    pub fn ensure_keys_prepare<I>(
        gpu: &AwsmRendererWebGpu,
        shaders: &Shaders,
        pipeline_layouts: &PipelineLayouts,
        cache_keys: I,
    ) -> Result<ComputePipelinesPrepWithPromises>
    where
        I: IntoIterator<Item = ComputePipelineCacheKey>,
    {
        // Each cache key is allocated/cloned exactly once — when it
        // crosses the IntoIterator boundary into `inputs`. Cache misses
        // keep the key alive in `inputs` until install time, then move
        // it into `self.cache` via `Option::take`. See
        // `RenderPipelines::ensure_keys_prepare` for the longer rationale.
        let inputs: Vec<ComputePipelineCacheKey> = cache_keys.into_iter().collect();
        let slot: Vec<Option<ComputePipelineKey>> = vec![None; inputs.len()];

        // We can't read `self.cache` from a static prep method;
        // callers must pass `cache_keys` that they want compiled.
        // The async wrapper [`Self::ensure_keys`] does the cache-hit
        // pre-pass via a separate call. For now `prepare` treats every
        // input as pending and lets `install` overwrite cache entries
        // (idempotent: re-inserts the same SlotMap key on hit).
        //
        // The original `ensure_keys` had cache-hit shortcuts —
        // preserved in the wrapper below by checking `self.cache` first
        // and emitting a tighter `prepare` only over the misses.
        //
        // **Within-batch dedup**: see the corresponding pre-pass in
        // [`Self::ensure_keys`]. `prepare` keeps its one-to-one
        // input-to-promise contract so direct callers (the
        // pipeline_scheduler launch path) that zip
        // `(promise_jobs, prep.promises)` keep working. The wrapper
        // dedups BEFORE calling `prepare` and fans the resolved key
        // back out to every input slot via the prep's own
        // `pending_targets`. Launch sites build `promise_jobs` from
        // distinct `(slot, cache_key)` tuples — duplicate cache keys
        // there would be a bug, not a perf concern.
        let pending_input_indices: Vec<usize> = (0..inputs.len()).collect();
        let pending_targets: Vec<Vec<usize>> = (0..inputs.len()).map(|i| vec![i]).collect();

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
        // https://github.com/dakom/awsm-renderer/pull/99). Each individual future is
        // wrapped to log on resolve with its finish-order index and
        // cumulative wall-clock since batch start. **Critically:** the
        // wrapping uses an `async move { ... promise.await ... }` block
        // per promise — the wrapper / scheduler still drives every
        // future concurrently. Do NOT replace `join_all(promises).await`
        // with a serial loop: that defeats Dawn's parallel compile pool.
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
        let finish_times: std::rc::Rc<std::cell::RefCell<Vec<(usize, f64, bool)>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::with_capacity(n)));
        let promises: Vec<
            Pin<
                Box<dyn Future<Output = std::result::Result<web_sys::GpuComputePipeline, JsValue>>>,
            >,
        > = descriptors
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let label = labels[i].clone();
                let total = n;
                let promise = JsFuture::from(gpu.create_compute_pipeline_promise(d));
                let ft = finish_times.clone();
                let fut = async move {
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
                };
                Box::pin(fut)
                    as Pin<
                        Box<
                            dyn Future<
                                Output = std::result::Result<web_sys::GpuComputePipeline, JsValue>,
                            >,
                        >,
                    >
            })
            .collect();

        Ok(ComputePipelinesPrepWithPromises {
            prep: ComputePipelinesPrep {
                inputs,
                slot,
                pending_input_indices,
                pending_targets,
                labels,
                t_start,
                finish_times,
            },
            promises,
        })
    }

    /// Sync phase 2 of [`Self::ensure_keys`] — takes the awaited
    /// results and installs them into the slotmap + cache. Emits the
    /// batch summary + finish-order log on the way out.
    pub fn ensure_keys_install(
        &mut self,
        prep: ComputePipelinesPrep,
        results: Vec<std::result::Result<web_sys::GpuComputePipeline, JsValue>>,
    ) -> Result<Vec<ComputePipelineKey>> {
        let ComputePipelinesPrep {
            inputs,
            mut slot,
            pending_input_indices,
            pending_targets,
            labels,
            t_start,
            finish_times,
        } = prep;

        let n = pending_input_indices.len();
        let dt_ms = web_sys::js_sys::Date::now() - t_start;
        tracing::info!(
            target: "awsm_renderer::boot_timing",
            "ComputePipelines::ensure_keys: {n} pipelines compiled in {dt_ms:.0}ms",
        );
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
            // Cache-hit early-rejoin path: if the cache already has the
            // key (e.g. a parallel-submit raced to install first), skip
            // the duplicate install.
            let cache_key_ref = inputs_owned[input_idx]
                .as_ref()
                .expect("pending input slot must own its key");
            if let Some(existing) = self.cache.get(cache_key_ref).copied() {
                for i in input_targets {
                    slot[i] = Some(existing);
                }
                // Drop the now-unused descriptor result if it was Ok;
                // the cache hit wins (the racing promise's pipeline is
                // discarded silently).
                let _ = result;
                continue;
            }
            let pipeline: web_sys::GpuComputePipeline = result
                .map_err(|e| AwsmComputePipelineError::Core(AwsmCoreError::pipeline_creation(e)))?;
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

    /// Sync cache lookup — returns the `ComputePipelineKey` for an
    /// already-cached cache key, or `None` if the cache doesn't
    /// contain it. Used by the literal-push-futures launch path
    /// (Block D.1 PART 2) to skip already-installed sub-pipelines
    /// before kicking off a new compile.
    pub fn cache_lookup(&self, cache_key: &ComputePipelineCacheKey) -> Option<&ComputePipelineKey> {
        self.cache.get(cache_key)
    }

    /// Sync install of a resolved `GpuComputePipeline` — inserts into
    /// the slotmap, registers the cache_key → key mapping, returns
    /// the new `ComputePipelineKey`. Used by
    /// `AwsmRenderer::apply_compile_resolution` (Block D.1 PART 2)
    /// when a pushed compile future resolves. Idempotent on cache
    /// hits: if `cache_key` is already in the cache, returns the
    /// existing key + drops the parameter `pipeline` (a parallel
    /// submit raced and won).
    pub fn install_resolved_pipeline(
        &mut self,
        pipeline: web_sys::GpuComputePipeline,
        cache_key: ComputePipelineCacheKey,
    ) -> ComputePipelineKey {
        if let Some(existing) = self.cache.get(&cache_key) {
            return *existing;
        }
        let key = self.lookup.insert(pipeline);
        self.cache.insert(cache_key, key);
        key
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

    let mut programmable_stage =
        ProgrammableStage::new(shader_module, cache_key.entry_point.as_deref());
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
    /// Entry-point override. `None` uses the shader module's single
    /// `@compute` entry point (the common case). `Some(name)` selects a
    /// specific entry point — required when one module exposes multiple
    /// `@compute` functions (e.g. the light-culling pass's two-stage
    /// `cs_tile` / `cs_main`), and part of the cache key so the distinct
    /// pipelines don't collide.
    pub entry_point: Option<String>,
}

impl ComputePipelineCacheKey {
    /// Creates a cache key with shader and layout keys.
    pub fn new(shader_key: ShaderKey, layout_key: PipelineLayoutKey) -> Self {
        Self {
            shader_key,
            layout_key,
            constant_overrides: BTreeMap::new(),
            entry_point: None,
        }
    }

    /// Selects a specific `@compute` entry point in the shader module.
    /// Use when the module exposes more than one compute function.
    pub fn with_entry_point(mut self, entry_point: &str) -> Self {
        self.entry_point = Some(entry_point.to_string());
        self
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
