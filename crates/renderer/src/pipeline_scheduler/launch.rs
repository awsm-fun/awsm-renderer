//! Block D.1 PART 2 — sync compile-launch path that pushes real
//! compile promises into the scheduler's `inflight_compile` queue.
//!
//! The architecture per the plan doc's § Block D prescription:
//!
//! 1. **Sync at submit time** — `AwsmRenderer::launch_dynamic_material_compile`
//!    is called when a new dynamic material registers. It synchronously:
//!    - Installs every shader needed (via
//!      `Shaders::ensure_keys_sync_skip_validate`). `compile_shader`
//!      is a sync browser call; validation is skipped here and
//!      surfaces later as a `create_compute_pipeline_async` rejection.
//!    - Builds all pipeline cache keys + descriptors (sync — needs
//!      `&Shaders` + `&PipelineLayouts`).
//!    - Issues every `gpu.create_compute_pipeline_promise` back-to-back
//!      (sync — Dawn starts compiling all N in parallel by the time the
//!      loop returns).
//!    - Wraps each `JsFuture` in a closure that emits a
//!      [`PipelineCompileResolution`] when the pipeline resolves; pushes
//!      it into the scheduler via `push_compile_future`.
//!
//! 2. **`AwsmRenderer::poll_pipeline_scheduler`** drains the
//!    scheduler's `inflight_compile` queue per-frame and calls
//!    `apply_compile_resolution` to install the resolved pipeline
//!    into the per-pass cache + decrement the scheduler's
//!    sub-compile counter. When the last sub-pipeline lands, the
//!    material flips Pending → Ready and frontends subscribed to the
//!    status stream observe the transition.

use awsm_materials::{MaterialAlphaMode, MaterialShaderId};
use wasm_bindgen::JsValue;

use crate::pipeline_scheduler::{CompileInstallTarget, PipelineCompileResolution, PipelineGroupId};
use crate::pipelines::compute_pipeline::ComputePipelineCacheKey;
use crate::render_passes::material_classify::shader::cache_key::ShaderCacheKeyMaterialClassify;
use crate::render_passes::material_opaque::shader::cache_key::{
    DynamicShaderInfo, ShaderCacheKeyMaterialOpaque,
};

impl crate::AwsmRenderer {
    /// Block D.1 PART 2 — kick off real-future compile for the
    /// scheduler entry corresponding to `shader_id`. Synchronously
    /// builds + issues every classify and opaque-compute pipeline
    /// promise; pushes each to the scheduler's `inflight_compile`.
    /// Returns immediately; the resulting material entry stays
    /// `Pending` until each sub-pipeline resolves.
    ///
    /// Idempotent on cache hits: cache-hit pipelines bypass the
    /// scheduler's sub-compile counter entirely (they're installed
    /// inline as part of this method's sync window). The counter
    /// only tracks actual in-flight async compiles.
    ///
    /// Skipped silently if the material has no scheduler entry
    /// (caller never called `submit_pipeline_group_batch` /
    /// `submit_dynamic_material` / `register_material`'s A.1 bridge).
    pub fn launch_dynamic_material_compile(
        &mut self,
        shader_id: MaterialShaderId,
    ) -> Result<(), crate::error::AwsmError> {
        // Find the scheduler MaterialId for this shader_id.
        let Some(mid) = self
            .pipeline_scheduler
            .find_material_by_shader_id(shader_id)
        else {
            return Ok(());
        };
        let group_id = PipelineGroupId::Material(mid);

        // Snapshot the active dispatch_hash + bucket_entries (cached
        // refreshes via the registry's per-mutation refresh).
        let entries = self.dynamic_materials.bucket_entries_cached().to_vec();
        let dispatch_hash = self.dynamic_materials.dispatch_hash_cached();

        // Resolve cached pipeline layouts (sync).
        let classify_bg = &self.render_passes.material_classify.bind_groups;
        let classify_layout_msaa = self.pipeline_layouts.get_key(
            &self.gpu,
            &self.bind_group_layouts,
            crate::pipeline_layouts::PipelineLayoutCacheKey::new(vec![
                classify_bg.multisampled_bind_group_layout_key,
            ]),
        )?;
        let classify_layout_no_msaa = self.pipeline_layouts.get_key(
            &self.gpu,
            &self.bind_group_layouts,
            crate::pipeline_layouts::PipelineLayoutCacheKey::new(vec![
                classify_bg.singlesampled_bind_group_layout_key,
            ]),
        )?;
        let opaque_bg = &self.render_passes.material_opaque.bind_groups;
        let texture_pool_arrays_len = opaque_bg.texture_pool_arrays_len;
        let texture_pool_samplers_len = opaque_bg.texture_pool_sampler_keys.len() as u32;
        let opaque_layout_msaa = self.pipeline_layouts.get_key(
            &self.gpu,
            &self.bind_group_layouts,
            crate::pipeline_layouts::PipelineLayoutCacheKey::new(vec![
                opaque_bg.multisampled_main_bind_group_layout_key,
                opaque_bg.lights_bind_group_layout_key,
                opaque_bg.texture_pool_textures_bind_group_layout_key,
                opaque_bg.shadows_bind_group_layout_key,
            ]),
        )?;
        let opaque_layout_no_msaa = self.pipeline_layouts.get_key(
            &self.gpu,
            &self.bind_group_layouts,
            crate::pipeline_layouts::PipelineLayoutCacheKey::new(vec![
                opaque_bg.singlesampled_main_bind_group_layout_key,
                opaque_bg.lights_bind_group_layout_key,
                opaque_bg.texture_pool_textures_bind_group_layout_key,
                opaque_bg.shadows_bind_group_layout_key,
            ]),
        )?;

        // Build shader cache keys + slot identities. For this
        // shader_id only — other already-registered materials keep
        // their existing pipelines (the new material entering shifts
        // dispatch_hash, so classify recompiles for both MSAA states).
        // `dynamic_material_registration` lives at the AwsmRenderer
        // facade; the `DynamicMaterials` registry exposes the lookup
        // via `.get()`. Take a short-lived borrow + clone for the
        // shader-info build (the registration's WGSL fragment is
        // captured by the dynamic_shader info below).
        let reg = match self.dynamic_materials.get(shader_id) {
            Some(r) => r.clone(),
            None => return Ok(()),
        };
        let dynamic_shader = Some(DynamicShaderInfo {
            struct_decl: awsm_materials::dynamic_layout::generate_wgsl_struct(
                "MaterialData",
                &reg.layout,
            ),
            loader_decl: awsm_materials::dynamic_layout::generate_wgsl_loader(
                "MaterialData",
                "material_data_load",
                &reg.layout,
            ),
            wgsl_fragment: reg.wgsl_fragment.clone(),
        });

        let mut shader_jobs: Vec<crate::shaders::ShaderCacheKey> = Vec::new();
        let mut slots: Vec<LaunchSlot> = Vec::new();

        // Classify variants (per MSAA × emit_edge_data toggle).
        for msaa in [Some(4u32), None] {
            shader_jobs.push(
                ShaderCacheKeyMaterialClassify {
                    msaa_sample_count: msaa,
                    bucket_entries: entries.clone(),
                    emit_edge_data: msaa.is_some() && crate::edge_resolve_supported(&self.gpu),
                }
                .into(),
            );
            slots.push(LaunchSlot::Classify { msaa });
        }
        // Opaque variants for THIS shader_id.
        for &(msaa, mipmaps) in &[
            (Some(4u32), true),
            (Some(4u32), false),
            (None, true),
            (None, false),
        ] {
            shader_jobs.push(
                ShaderCacheKeyMaterialOpaque {
                    texture_pool_arrays_len,
                    texture_pool_samplers_len,
                    msaa_sample_count: msaa,
                    mipmaps,
                    shader_id,
                    dispatch_hash,
                    dynamic_shader: dynamic_shader.clone(),
                    bucket_entries: entries.clone(),
                }
                .into(),
            );
            slots.push(LaunchSlot::Opaque { msaa, mipmaps });
        }
        // Transparent stubs handled by the legacy prewarm path (per-mesh,
        // depends on buffer_info — not part of this launch).
        if reg.alpha_mode == MaterialAlphaMode::Blend {
            tracing::debug!(
                target: "awsm_renderer::pipeline_readiness",
                "launch_dynamic_material_compile: Blend material — transparent pipeline stays on prewarm path",
            );
        }

        // Sync shader install — skips validation; validation errors
        // re-surface as pipeline-creation rejections below.
        let resolved_shader_keys = self
            .shaders
            .ensure_keys_sync_skip_validate(&self.gpu, shader_jobs)?;

        // Build compute pipeline cache keys per slot.
        let mut compute_jobs: Vec<(LaunchSlot, ComputePipelineCacheKey)> =
            Vec::with_capacity(slots.len());
        for (shader_key, slot) in resolved_shader_keys.into_iter().zip(slots) {
            let layout = match &slot {
                LaunchSlot::Classify { msaa } => {
                    if msaa.is_some() {
                        classify_layout_msaa
                    } else {
                        classify_layout_no_msaa
                    }
                }
                LaunchSlot::Opaque { msaa, .. } => {
                    if msaa.is_some() {
                        opaque_layout_msaa
                    } else {
                        opaque_layout_no_msaa
                    }
                }
            };
            compute_jobs.push((slot, ComputePipelineCacheKey::new(shader_key, layout)));
        }

        // Cache-hit dedup pass: install hits inline, defer misses to
        // the scheduler's inflight_compile queue.
        let mut promise_jobs: Vec<(LaunchSlot, ComputePipelineCacheKey)> = Vec::new();
        for (slot, cache_key) in &compute_jobs {
            if let Some(existing_key) = self.pipelines.compute.cache_lookup(cache_key).copied() {
                install_per_pass(self, slot, shader_id, dispatch_hash, existing_key);
            } else {
                promise_jobs.push((slot.clone(), cache_key.clone()));
            }
        }

        if promise_jobs.is_empty() {
            // Every variant was a cache hit. Manually decrement +
            // mark Ready (the counter never incremented). Just call
            // mark_ready directly.
            self.pipeline_scheduler.mark_ready(group_id);
            return Ok(());
        }

        // Pre-compute the snapshot of state needed at launch time
        // (the prepare step needs &Shaders + &PipelineLayouts; the
        // returned promises are 'static so they outlive this borrow).
        let cache_keys_only: Vec<ComputePipelineCacheKey> =
            promise_jobs.iter().map(|(_, k)| k.clone()).collect();
        let mut prepped =
            crate::pipelines::compute_pipeline::ComputePipelines::ensure_keys_prepare(
                &self.gpu,
                &self.shaders,
                &self.pipeline_layouts,
                cache_keys_only.clone(),
            )?;

        // The factored ensure_keys_prepare treats every input as a miss;
        // prep.promises has the same length as promise_jobs.
        let generation = match self.pipeline_scheduler.material_generation(mid) {
            Some(g) => g,
            None => return Ok(()),
        };
        let promises = std::mem::take(&mut prepped.promises);

        for ((slot, cache_key), promise) in promise_jobs.into_iter().zip(promises) {
            let target = match &slot {
                LaunchSlot::Classify { msaa } => CompileInstallTarget::ClassifyDynamic {
                    dispatch_hash,
                    msaa: *msaa,
                },
                LaunchSlot::Opaque { msaa, mipmaps } => CompileInstallTarget::OpaqueDynamic {
                    shader_id,
                    msaa: *msaa,
                    mipmaps: *mipmaps,
                },
            };
            let id = group_id;
            let cache_key_clone = cache_key.clone();
            let fut: std::pin::Pin<
                Box<dyn std::future::Future<Output = PipelineCompileResolution>>,
            > = Box::pin(async move {
                let result: std::result::Result<web_sys::GpuComputePipeline, JsValue> =
                    promise.await;
                PipelineCompileResolution {
                    id,
                    generation,
                    target,
                    cache_key: cache_key_clone,
                    result,
                }
            });
            self.pipeline_scheduler.push_compile_future(group_id, fut);
        }
        tracing::info!(
            target: "awsm_renderer::pipeline_readiness",
            "launch_dynamic_material_compile: {:?} sub-pipelines pushed for material({:?})",
            cache_keys_only.len(),
            mid,
        );
        Ok(())
    }

    /// Drain ONE pipeline compile resolution from the scheduler's
    /// inflight_compile (non-blocking), install + decrement the
    /// sub-compile counter. Returns `true` when a resolution was
    /// applied; `false` when the queue is empty / no resolutions are
    /// ready. Called by `poll_pipeline_scheduler` per-frame.
    pub fn apply_compile_resolution(&mut self) -> bool {
        let Some(resolution) = self.pipeline_scheduler.next_compile_resolution() else {
            return false;
        };
        self.apply_compile_resolution_inline(resolution);
        true
    }

    /// Install a single resolved compile (Block D.1 PART 2). Sync;
    /// caller already obtained the resolution via either
    /// `next_compile_resolution` (non-blocking) or
    /// `inflight_compile.next().await` (the `wait_for_pipelines_ready`
    /// path). Decrements the material's sub-compile counter; when
    /// the counter hits 0 the material transitions `Pending → Ready`
    /// and emits a status event.
    pub(crate) fn apply_compile_resolution_inline(
        &mut self,
        resolution: PipelineCompileResolution,
    ) {
        let PipelineCompileResolution {
            id,
            generation,
            target,
            cache_key,
            result,
        } = resolution;
        let mid = match id {
            PipelineGroupId::Material(mid) => mid,
            PipelineGroupId::Pass(_) => {
                tracing::warn!(
                    target: "awsm_renderer::pipeline_readiness",
                    "apply_compile_resolution: Pass-flavoured resolution not yet supported"
                );
                return;
            }
        };
        // Stale-generation drop: a config-flip or unregister bumped
        // generation between submit + resolve; the resolved pipeline
        // would be installed against an outdated config.
        if self
            .pipeline_scheduler
            .material_generation(mid)
            .map(|g| g != generation)
            .unwrap_or(true)
        {
            tracing::debug!(
                target: "awsm_renderer::pipeline_readiness",
                "apply_compile_resolution: stale resolution dropped (material({:?}))",
                mid
            );
            return;
        }

        let pipeline = match result {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    target: "awsm_renderer::pipeline_readiness",
                    "apply_compile_resolution: pipeline-creation failed for material({:?}): {:?}",
                    mid,
                    e,
                );
                self.pipeline_scheduler.mark_failed(
                    id,
                    crate::error::AwsmError::PipelineVariantNotCompiled(
                        "create_compute_pipeline_async rejected",
                    ),
                );
                return;
            }
        };

        // Install: slotmap + cache + per-pass.
        let pipeline_key = self
            .pipelines
            .compute
            .install_resolved_pipeline(pipeline, cache_key);
        install_per_pass(
            self,
            &launch_slot_from_target(&target),
            shader_id_from_target(&target),
            dispatch_hash_from_target(&target),
            pipeline_key,
        );

        self.pipeline_scheduler.note_subcompile_complete(mid);
    }
}

#[derive(Clone)]
enum LaunchSlot {
    Classify { msaa: Option<u32> },
    Opaque { msaa: Option<u32>, mipmaps: bool },
}

fn launch_slot_from_target(t: &CompileInstallTarget) -> LaunchSlot {
    match t {
        CompileInstallTarget::ClassifyDynamic { msaa, .. } => LaunchSlot::Classify { msaa: *msaa },
        CompileInstallTarget::OpaqueDynamic { msaa, mipmaps, .. } => LaunchSlot::Opaque {
            msaa: *msaa,
            mipmaps: *mipmaps,
        },
    }
}

fn shader_id_from_target(t: &CompileInstallTarget) -> MaterialShaderId {
    match t {
        CompileInstallTarget::OpaqueDynamic { shader_id, .. } => *shader_id,
        // Classify target doesn't carry shader_id; sentinel value
        // (the install path doesn't read it for classify).
        CompileInstallTarget::ClassifyDynamic { .. } => MaterialShaderId::PBR,
    }
}

fn dispatch_hash_from_target(t: &CompileInstallTarget) -> u64 {
    match t {
        CompileInstallTarget::ClassifyDynamic { dispatch_hash, .. } => *dispatch_hash,
        CompileInstallTarget::OpaqueDynamic { .. } => 0,
    }
}

fn install_per_pass(
    renderer: &mut crate::AwsmRenderer,
    slot: &LaunchSlot,
    shader_id: MaterialShaderId,
    dispatch_hash: u64,
    pipeline_key: crate::pipelines::compute_pipeline::ComputePipelineKey,
) {
    use crate::render_passes::material_opaque::pipeline::PipelineKeyId;
    match slot {
        LaunchSlot::Classify { msaa } => {
            renderer
                .render_passes
                .material_classify
                .dynamic_pipeline_cache
                .borrow_mut()
                .insert((dispatch_hash, *msaa), pipeline_key);
        }
        LaunchSlot::Opaque { msaa, mipmaps } => {
            renderer
                .render_passes
                .material_opaque
                .pipelines
                .insert_dynamic_pipeline(
                    PipelineKeyId {
                        msaa_sample_count: *msaa,
                        mipmaps: *mipmaps,
                        shader_id,
                    },
                    pipeline_key,
                );
        }
    }
}
