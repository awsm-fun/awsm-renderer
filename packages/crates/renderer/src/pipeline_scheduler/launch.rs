//! Block D.1 PART 2 — sync compile-launch path that pushes real
//! compile promises into the scheduler's `inflight_compile` queue.
//!
//! The architecture:
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

use crate::pipeline_scheduler::{
    CompileInstallTarget, PassKind, PipelineCompileResolution, PipelineGroupId,
};
use crate::pipelines::compute_pipeline::ComputePipelineCacheKey;
use crate::render_passes::material_classify::shader::cache_key::ShaderCacheKeyMaterialClassify;
use crate::render_passes::material_opaque::edge_pipeline::{
    EdgePipelineSlot, EdgeResolvePipelineKeyId,
};
use crate::render_passes::material_opaque::shader::cache_key::{
    DynamicShaderInfo, ShaderCacheKeyMaterialOpaque,
};

impl crate::AwsmRenderer {
    /// Block D.1 PART 2 — kick off real-future compile for the
    /// scheduler entry corresponding to `shader_id`. Synchronously
    /// builds + issues this material's classify + opaque-compute
    /// pipeline promises; pushes each to the scheduler's
    /// `inflight_compile`. Returns immediately; the resulting material
    /// entry stays `Pending` until every sub-pipeline resolves.
    ///
    /// **MSAA-edge integration**: this method does NOT launch edge-resolve
    /// pipelines. Edge resolve is a LAYOUT-level concern (its cache keys
    /// embed the whole `bucket_entries`), so it is rebuilt ONCE per layout
    /// change by [`Self::launch_edge_resolve_compile`] from the relaunch
    /// sites — not once per material. `render_edge_resolve` is
    /// per-bucket-independent (no all-or-nothing gate); a bucket whose edge
    /// pipeline isn't resident yet simply keeps primary-pass shading for
    /// the frame.
    ///
    /// Idempotent on cache hits: cache-hit pipelines bypass the
    /// scheduler's sub-compile counter entirely (they're installed
    /// inline as part of this method's sync window). The counter
    /// only tracks actual in-flight async compiles. If every
    /// sub-pipeline was a cache hit, the method calls `mark_ready`
    /// inline before returning so the status surface stays accurate.
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

        // Capture generation up front. `&mut self` for the whole
        // method body means nothing concurrent can bump it; once
        // we've passed this guard, all later code can rely on the
        // captured value. Doing this BEFORE any state mutation
        // (waiter registration, promise pushes) means we never leak
        // counter/waiter state if the material is somehow already
        // gone.
        let Some(generation) = self.pipeline_scheduler.material_generation(mid) else {
            return Ok(());
        };

        // Compile ONLY the variant matching the live AA config — the dispatch
        // only ever uses `(active_msaa, active_mipmaps)`, and `set_anti_aliasing`
        // relaunches every registered material for the new config on a toggle
        // (already-compiled variants stay cached). Matches the cold-boot
        // lazy-pool model; compiling all 4 (msaa × mipmap) here was redundant.
        let active_msaa = self.anti_aliasing.msaa_sample_count;
        let active_mipmaps = self.anti_aliasing.mipmap;

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
        // `reg` is `Some` only for a custom author material; a first-party
        // feature-set variant has a dynamic-range id but no registration —
        // it compiles the built-in PBR/Toon body (no `DynamicShaderInfo`).
        let reg = self.dynamic_materials.get(shader_id).cloned();
        // Nothing to compile — the id was removed between submit and launch
        // (an orphan; see `is_launchable_material`). Defense-in-depth: the
        // relaunch sites retire orphans before they reach here, but mark any
        // group still `Pending` Ready so a stray caller can't hang the
        // compile-status surface.
        if !self.is_launchable_material(shader_id) {
            if let Some(mid) = self
                .pipeline_scheduler
                .find_material_by_shader_id(shader_id)
            {
                self.pipeline_scheduler
                    .mark_ready(PipelineGroupId::Material(mid));
            }
            return Ok(());
        }
        let dynamic_shader = reg.as_ref().map(|reg| DynamicShaderInfo {
            shader_includes: reg.shader_includes.resolve(),
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
        // Shading family + feature mask + skybox ownership from the bucket
        // entry (Custom for a registration; Pbr/Toon for a variant).
        let (base, pbr_features, owns_skybox) = opaque_variant_params(&entries, shader_id);

        let mut shader_jobs: Vec<crate::shaders::ShaderCacheKey> = Vec::new();
        let mut slots: Vec<LaunchSlot> = Vec::new();

        // Classify variant for the ACTIVE MSAA only (the only one dispatched;
        // a toggle recompiles the other via `set_anti_aliasing`).
        {
            let msaa = active_msaa;
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
        // Opaque variants for THIS shader_id — only for materials that route
        // to the opaque pass. A Blend/Mask *dynamic* material's author body
        // targets the transparent contract (returns `TransparentShadingOutput`),
        // so compiling it in the opaque wrapper fails; it renders via the
        // transparent pass instead. First-party variants (no registration)
        // always build — their built-in body fits both contracts.
        let build_opaque = reg
            .as_ref()
            .is_none_or(|r| matches!(r.alpha_mode, MaterialAlphaMode::Opaque));
        if build_opaque {
            // Active (msaa, mipmap) only — see the note at the top of this fn.
            let (msaa, mipmaps) = (active_msaa, active_mipmaps);
            shader_jobs.push(
                ShaderCacheKeyMaterialOpaque {
                    texture_pool_arrays_len,
                    texture_pool_samplers_len,
                    msaa_sample_count: msaa,
                    mipmaps,
                    shader_id,
                    base,
                    owns_skybox,
                    pbr_features,
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
        if reg.as_ref().map(|r| r.alpha_mode) == Some(MaterialAlphaMode::Blend) {
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

        // Cache-hit + cross-call waiter dedup. When an outer loop
        // (e.g. `register_material`'s relaunch over
        // `registered_material_shader_ids()`) calls this method N
        // times with overlapping classify cache keys (classify is
        // keyed on `(msaa, bucket_entries, emit_edge_data)`, NOT
        // shader_id, so every dynamic launch in the same loop wants
        // the SAME classify cache key), the first launch issues the
        // promise; subsequent launches register as additional
        // waiters (so their Ready transition waits on the shared
        // compile) and skip the duplicate
        // `createComputePipelineAsync`.
        //
        // **Waiter registration is deferred until after the fallible
        // `ensure_keys_prepare` succeeds**: if prep returns Err,
        // no waiters/counters are touched, so a sync prep error
        // never leaks subcompile counters or in-flight waiter map
        // entries. Within this single launch invocation (single
        // sync window — no concurrent calls in single-threaded
        // wasm), the decide-vs-register split is safe: the
        // read-only `has_compute_compile_waiter` check correctly
        // sees what PRIOR launches have registered, and within
        // this launch the slot identities (msaa × mipmaps × type)
        // produce distinct cache keys so there's no
        // within-batch dedup needed at the decision step.
        let mut promise_jobs: Vec<(LaunchSlot, ComputePipelineCacheKey)> = Vec::new();
        let mut skip_keys: Vec<ComputePipelineCacheKey> = Vec::new();
        for (slot, cache_key) in &compute_jobs {
            if let Some(existing_key) = self.pipelines.compute.cache_lookup(cache_key).copied() {
                install_per_pass(self, slot, shader_id, dispatch_hash, existing_key);
            } else if self
                .pipeline_scheduler
                .has_compute_compile_waiter(cache_key)
            {
                // Another launch's promise is already in flight for
                // this cache key. Defer the waiter registration to
                // AFTER prep succeeds (no prep call needed for skips,
                // but we still want the same all-or-nothing
                // registration semantics across the launch).
                skip_keys.push(cache_key.clone());
            } else {
                promise_jobs.push((slot.clone(), cache_key.clone()));
            }
        }

        let opaque_promise_count = promise_jobs.len();

        // Sync prep BEFORE any waiter registration so a prep error
        // doesn't leak counter / waiter-map state. `Option<Prepped>`
        // is `None` when there are no promises to push (everything
        // was cache-hit or skip-via-in-flight).
        let prepped_opt = if !promise_jobs.is_empty() {
            let cache_keys_only: Vec<ComputePipelineCacheKey> =
                promise_jobs.iter().map(|(_, k)| k.clone()).collect();
            Some(
                crate::pipelines::compute_pipeline::ComputePipelines::ensure_keys_prepare(
                    &self.gpu,
                    &self.shaders,
                    &self.pipeline_layouts,
                    cache_keys_only,
                )?,
            )
        } else {
            None
        };

        // Prep (if any) succeeded — NOW commit waiter registrations
        // for skip keys AND push keys. After this point the path is
        // infallible: every counter bump matches a future push that
        // drains via `apply_compile_resolution_inline`.
        for key in &skip_keys {
            self.pipeline_scheduler
                .register_compute_compile_waiter(key.clone(), mid);
        }
        for (_, key) in &promise_jobs {
            self.pipeline_scheduler
                .register_compute_compile_waiter(key.clone(), mid);
        }

        if let Some(mut prepped) = prepped_opt {
            // The factored ensure_keys_prepare treats every input as
            // a miss; prep.promises has the same length as promise_jobs.
            // `generation` was captured at function top.
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
                let fut: std::pin::Pin<
                    Box<dyn std::future::Future<Output = PipelineCompileResolution>>,
                > = Box::pin(async move {
                    let result: std::result::Result<web_sys::GpuComputePipeline, JsValue> =
                        promise.await;
                    PipelineCompileResolution {
                        id,
                        generation,
                        target,
                        cache_key,
                        result,
                    }
                });
                self.pipeline_scheduler.push_compile_future_no_count(fut);
            }
            tracing::info!(
                target: "awsm_renderer::pipeline_readiness",
                "launch_dynamic_material_compile: {:?} opaque/classify sub-pipelines pushed for material({:?})",
                opaque_promise_count,
                mid,
            );
        }

        // NOTE: edge-resolve pipelines are NOT launched here. They are a
        // layout-level concern (their cache keys embed the whole
        // bucket_entries), rebuilt ONCE per layout change by
        // `launch_edge_resolve_compile` from the relaunch sites — not once
        // per material. See that method's rustdoc.

        // If both launches were full cache hits, no promise pushed
        // → subcompile counter stayed at 0 → Ready never auto-fires
        // via note_subcompile_complete. Mark Ready inline.
        if self.pipeline_scheduler.pending_subcompile_count(mid) == 0 {
            self.pipeline_scheduler.mark_ready(group_id);
        }
        Ok(())
    }

    /// Block D.1 PART 2 first-party extension: kick off real-future
    /// compile for a FIRST-PARTY material's opaque pipeline.
    /// Mirrors [`Self::launch_dynamic_material_compile`] but with
    /// `dynamic_shader: None` and a hard-coded first-party shader_id
    /// (PBR / UNLIT / TOON / FLIPBOOK).
    ///
    /// Called from `AwsmRenderer::register_first_party_material` —
    /// the gltf-loader's first-party material insert path. Cold-boot
    /// no longer compiles first-party opaque pipelines (the eager
    /// `shader_descriptors_and_layouts` path now skips them); they
    /// land via this method as gltf materials register.
    ///
    /// **MSAA + mipmap variants**: compiles all 4 (msaa × mipmaps)
    /// combinations for this shader_id, so MSAA-flip + mipmap-flip
    /// don't trigger a fresh recompile. (Dynamic materials do the
    /// same.) The classify pipelines are NOT touched here — they're
    /// either already eager (no dynamic registered) or compiled via
    /// `launch_dynamic_material_compile` when dynamics enter.
    ///
    /// Idempotent on cache hits: variants already compiled are
    /// skipped; the scheduler counter only tracks actual in-flight
    /// async compiles.
    pub fn launch_first_party_material_compile(
        &mut self,
        shader_id: MaterialShaderId,
    ) -> Result<(), crate::error::AwsmError> {
        // Every dynamic-range id (custom author material OR a first-party
        // feature-set variant) routes through the dynamic launch: it
        // pushes the classify recompile that a bucket-list change needs.
        // The dynamic launch reads each bucket's `base`/`features` off its
        // registry entry and only attaches a `DynamicShaderInfo` for the
        // custom case (variants compile the built-in body). Canonical
        // first-party ids (PBR/UNLIT/TOON/FLIPBOOK) stay on this path.
        if shader_id.is_dynamic() {
            return self.launch_dynamic_material_compile(shader_id);
        }
        let Some(mid) = self
            .pipeline_scheduler
            .find_material_by_shader_id(shader_id)
        else {
            return Ok(());
        };
        let group_id = PipelineGroupId::Material(mid);

        // Capture generation up front — see the matching comment in
        // `launch_dynamic_material_compile` for the rationale (avoid
        // leaking waiter/counter state on a None lookup that happens
        // after waiter registration).
        let Some(generation) = self.pipeline_scheduler.material_generation(mid) else {
            return Ok(());
        };

        // Active (msaa, mipmap) only — the dispatch never uses a non-active
        // variant, and `set_anti_aliasing` relaunches this material for the new
        // config on a toggle (old variants stay cached).
        let active_msaa = self.anti_aliasing.msaa_sample_count;
        let active_mipmaps = self.anti_aliasing.mipmap;

        // First-party materials use the stable empty-state dispatch_hash
        // at registration time. The `dispatch_hash: 0` sentinel matches
        // what `MaterialOpaquePipelines::shader_descriptors_for_config_with`
        // used to emit in the eager batch.
        let dispatch_hash = self.dynamic_materials.dispatch_hash_cached();

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

        // Bucket entries snapshot — for first-party-only scenes
        // matches `first_party_bucket_entries()`; with dynamics
        // registered uses the live `bucket_entries_cached()`.
        let entries = self.dynamic_materials.bucket_entries_cached().to_vec();

        let mut shader_jobs: Vec<crate::shaders::ShaderCacheKey> = Vec::new();
        let mut slots: Vec<LaunchSlot> = Vec::new();

        // Read this bucket's shading family + specialized feature mask off
        // its registry bucket entry. For a per-feature-set PBR/Toon
        // variant this is `(Pbr|Toon, that variant's features)`; for a
        // canonical first-party id it's `(family, all())`. The opaque
        // template gates per-feature WGSL on these features (#16).
        let (base, pbr_features, owns_skybox) = opaque_variant_params(&entries, shader_id);

        // Active (msaa, mipmap) only — a toggle recompiles via `set_anti_aliasing`.
        let (msaa, mipmaps) = (active_msaa, active_mipmaps);
        shader_jobs.push(
            ShaderCacheKeyMaterialOpaque {
                texture_pool_arrays_len,
                texture_pool_samplers_len,
                msaa_sample_count: msaa,
                mipmaps,
                shader_id,
                base,
                owns_skybox,
                pbr_features,
                dispatch_hash,
                dynamic_shader: None,
                bucket_entries: entries.clone(),
            }
            .into(),
        );
        slots.push(LaunchSlot::Opaque { msaa, mipmaps });

        let resolved_shader_keys = self
            .shaders
            .ensure_keys_sync_skip_validate(&self.gpu, shader_jobs)?;

        let mut compute_jobs: Vec<(LaunchSlot, ComputePipelineCacheKey)> =
            Vec::with_capacity(slots.len());
        for (shader_key, slot) in resolved_shader_keys.into_iter().zip(slots) {
            let layout = match &slot {
                LaunchSlot::Opaque { msaa, .. } => {
                    if msaa.is_some() {
                        opaque_layout_msaa
                    } else {
                        opaque_layout_no_msaa
                    }
                }
                LaunchSlot::Classify { .. } => unreachable!("classify not emitted here"),
            };
            compute_jobs.push((slot, ComputePipelineCacheKey::new(shader_key, layout)));
        }

        // Cache-hit + cross-call waiter dedup. See the matching
        // block in `launch_dynamic_material_compile` for the full
        // rationale, including the "defer waiter registration until
        // after prep succeeds" ordering.
        let mut promise_jobs: Vec<(LaunchSlot, ComputePipelineCacheKey)> = Vec::new();
        let mut skip_keys: Vec<ComputePipelineCacheKey> = Vec::new();
        for (slot, cache_key) in &compute_jobs {
            if let Some(existing_key) = self.pipelines.compute.cache_lookup(cache_key).copied() {
                install_per_pass(self, slot, shader_id, dispatch_hash, existing_key);
            } else if self
                .pipeline_scheduler
                .has_compute_compile_waiter(cache_key)
            {
                skip_keys.push(cache_key.clone());
            } else {
                promise_jobs.push((slot.clone(), cache_key.clone()));
            }
        }

        let opaque_promise_count = promise_jobs.len();

        // Sync prep before any waiter registration.
        let prepped_opt = if !promise_jobs.is_empty() {
            let cache_keys_only: Vec<ComputePipelineCacheKey> =
                promise_jobs.iter().map(|(_, k)| k.clone()).collect();
            Some(
                crate::pipelines::compute_pipeline::ComputePipelines::ensure_keys_prepare(
                    &self.gpu,
                    &self.shaders,
                    &self.pipeline_layouts,
                    cache_keys_only,
                )?,
            )
        } else {
            None
        };

        // Now commit waiter registrations (skip + push). Past this
        // point everything is infallible.
        for key in &skip_keys {
            self.pipeline_scheduler
                .register_compute_compile_waiter(key.clone(), mid);
        }
        for (_, key) in &promise_jobs {
            self.pipeline_scheduler
                .register_compute_compile_waiter(key.clone(), mid);
        }

        if let Some(mut prepped) = prepped_opt {
            let promises = std::mem::take(&mut prepped.promises);
            for ((slot, cache_key), promise) in promise_jobs.into_iter().zip(promises) {
                let target = match &slot {
                    LaunchSlot::Opaque { msaa, mipmaps } => CompileInstallTarget::OpaqueDynamic {
                        shader_id,
                        msaa: *msaa,
                        mipmaps: *mipmaps,
                    },
                    LaunchSlot::Classify { .. } => unreachable!(),
                };
                let id = group_id;
                let fut: std::pin::Pin<
                    Box<dyn std::future::Future<Output = PipelineCompileResolution>>,
                > = Box::pin(async move {
                    let result: std::result::Result<web_sys::GpuComputePipeline, JsValue> =
                        promise.await;
                    PipelineCompileResolution {
                        id,
                        generation,
                        target,
                        cache_key,
                        result,
                    }
                });
                self.pipeline_scheduler.push_compile_future_no_count(fut);
            }
            tracing::info!(
                target: "awsm_renderer::pipeline_readiness",
                "launch_first_party_material_compile({:?}): {} opaque sub-pipelines pushed for material({:?})",
                shader_id,
                opaque_promise_count,
                mid,
            );
        }

        // NOTE: edge-resolve pipelines are NOT launched here — they are a
        // layout-level concern rebuilt once per layout change by
        // `launch_edge_resolve_compile` from the relaunch sites (see that
        // method's rustdoc).

        // If both launches were full cache hits, mark Ready inline.
        // Otherwise note_subcompile_complete will fire it when the
        // last sub-pipeline resolves.
        if self.pipeline_scheduler.pending_subcompile_count(mid) == 0 {
            self.pipeline_scheduler.mark_ready(group_id);
        }
        Ok(())
    }

    /// Rebuild the MSAA edge-resolve pipelines for the CURRENT bucket
    /// layout — the per-shader-id `edge_resolve` for **every** bucket +
    /// the global `skybox_edge_resolve` + global `final_blend` — by
    /// pushing their compile promises into the scheduler's
    /// `inflight_compile` queue.
    ///
    /// **Scope: this is about COMPILING the edge pipelines, NOT about how
    /// they're dispatched at render time.** The render-time scatter+gather
    /// is unchanged: `render_edge_resolve` still issues one indirect
    /// compute dispatch PER BUCKET, each consuming only that bucket's edge
    /// pixels (`per_shader_args_offset(bucket_index)`) and atomic-adding
    /// into the accumulator, then `final_blend`. That per-bucket-with-edges
    /// parallelism is untouched here.
    ///
    /// **What IS layout-level is the compilation.** Every edge shader's
    /// cache key embeds the full `bucket_entries` (the per-shader shaders
    /// template the bucket index / sample-list offsets; skybox +
    /// final_blend iterate every bucket), so a bucket-layout change
    /// invalidates ALL of the edge pipelines at once. We therefore COMPILE
    /// the whole set ONCE per layout change (from the two sync relaunch
    /// sites — `register_material`'s tail and
    /// `relaunch_all_buckets_after_layout_change`), building the descriptor
    /// set a single time (O(N) buckets). The old code instead re-ran this
    /// compile-launch once per material (each rebuilding the whole N-entry
    /// descriptor set, then filtering to one slot via an `only_shader_id`
    /// arg) = O(N²) descriptor builds per layout change, and made each
    /// per-shader pipeline's *install* hostage to a *different* material's
    /// generation. Same N pipelines, same per-bucket dispatch consuming
    /// them — just compiled in one batch instead of N overlapping ones.
    ///
    /// **Ownership / staleness — NO material, NO PBR.** Edge pipelines
    /// belong to the bucket LAYOUT, not to any material (a scene may have
    /// no PBR material at all — `AWSM_material_none` / unlit-only / custom-
    /// only — so there is no canonical material to anchor on). The promises
    /// are charged to the single non-material `PassKind::MaterialEdgeResolve`
    /// group, and their install is validated purely by LAYOUT-CONTENT: this
    /// call records the current layout's full edge key set via
    /// `MaterialEdgePipelines::set_desired_edge_keys`, and
    /// `apply_compile_resolution_inline` installs a resolved edge pipeline
    /// iff its key is still in that set (`is_edge_key_desired`) — otherwise
    /// the layout moved on and the resolution is dropped. No material
    /// generation is consulted. The async await paths
    /// (`MaterialEdgePipelines::ensure_compiled` from build / prewarm / AA +
    /// texture-pool changes) ALSO update the desired set and share
    /// `build_descriptors`, so the two paths can never diverge on which
    /// buckets get an edge pipeline.
    ///
    /// Idempotent: already-compiled keys install inline (cache hit) and
    /// already-in-flight keys are skipped (`edge_key_in_flight`), so a
    /// relaunch whose layout didn't actually change pays nothing and never
    /// double-compiles.
    ///
    /// No-op when MSAA is off or `edge_resolve_supported` is false.
    pub(crate) fn launch_edge_resolve_compile(&mut self) -> Result<(), crate::error::AwsmError> {
        if self.anti_aliasing.msaa_sample_count.is_none()
            || !crate::edge_resolve_supported(&self.gpu)
        {
            return Ok(());
        }

        let color_wgsl = awsm_renderer_core::texture::texture_format_to_wgsl_storage(
            self.render_textures.formats.color,
        )?;
        let bucket_entries = self.dynamic_materials.bucket_entries_cached().to_vec();

        // Build sync descriptors via the edge_pipelines helper. The
        // pipeline-layout keys are committed onto edge_pipelines as a
        // side-effect (so dispatch sites observe the live layouts).
        let descs = match self
            .render_passes
            .material_opaque
            .edge_pipelines
            .build_descriptors(
                &self.gpu,
                &mut self.pipeline_layouts,
                &mut self.bind_group_layouts,
                &self.render_passes.material_opaque.bind_groups,
                &self.render_passes.material_opaque.edge_bind_group_layouts,
                &bucket_entries,
                &self.anti_aliasing,
                color_wgsl,
                Some(&self.dynamic_materials),
            )? {
            Some(d) => d,
            None => return Ok(()),
        };

        // Sync shader install (validation skipped — surfaces later
        // as a create_compute_pipeline_async rejection if the shader
        // is broken).
        let resolved_shader_keys = self
            .shaders
            .ensure_keys_sync_skip_validate(&self.gpu, descs.shader_cache_keys)?;

        // Build compute pipeline cache keys per entry.
        let mut compute_jobs: Vec<(EdgeLaunchSlot, ComputePipelineCacheKey)> =
            Vec::with_capacity(descs.slots.len());
        for ((shader_key, layout_key), slot) in resolved_shader_keys
            .into_iter()
            .zip(descs.pipeline_layout_keys.iter().copied())
            .zip(descs.slots.iter().copied())
        {
            let edge_slot = EdgeLaunchSlot(slot);
            compute_jobs.push((
                edge_slot,
                ComputePipelineCacheKey::new(shader_key, layout_key),
            ));
        }

        // Record what THIS layout wants — the authoritative install-validity
        // signal for resolved edge compiles. A promise that resolves later
        // installs iff its key is still in this set (i.e. its layout is still
        // current); otherwise it's dropped. Replaces any prior layout's set.
        self.render_passes
            .material_opaque
            .edge_pipelines
            .set_desired_edge_keys(compute_jobs.iter().map(|(_, k)| k.clone()));

        // Per key: already compiled → install inline (cache hit); already
        // in flight (an earlier layout-change launch this window) → skip;
        // else → schedule a compile promise. No material owner / waiter
        // bookkeeping — edge pipelines are layout-level, deduped here by the
        // edge cache itself + the in-flight set.
        let mut promise_jobs: Vec<(EdgeLaunchSlot, ComputePipelineCacheKey)> = Vec::new();
        for (slot, cache_key) in &compute_jobs {
            if let Some(existing_key) = self.pipelines.compute.cache_lookup(cache_key).copied() {
                install_edge_per_pass(self, &slot.0, existing_key);
            } else if self
                .render_passes
                .material_opaque
                .edge_pipelines
                .edge_key_in_flight(cache_key)
            {
                // A prior launch this window already has this compile in
                // flight; its single resolution installs for the layout.
            } else {
                promise_jobs.push((slot.clone(), cache_key.clone()));
            }
        }

        if promise_jobs.is_empty() {
            // Every pipeline was a cache hit or already in flight.
            return Ok(());
        }

        // Sync prep (issues the create-pipeline promises). Done before we
        // record in-flight markers so a prep error can't leak state.
        let cache_keys_only: Vec<ComputePipelineCacheKey> =
            promise_jobs.iter().map(|(_, k)| k.clone()).collect();
        let mut prepped =
            crate::pipelines::compute_pipeline::ComputePipelines::ensure_keys_prepare(
                &self.gpu,
                &self.shaders,
                &self.pipeline_layouts,
                cache_keys_only,
            )?;
        let promise_jobs_count = promise_jobs.len();
        let promises = std::mem::take(&mut prepped.promises);

        for ((slot, cache_key), promise) in promise_jobs.into_iter().zip(promises) {
            self.render_passes
                .material_opaque
                .edge_pipelines
                .mark_edge_key_in_flight(cache_key.clone());
            let target = match slot.0 {
                EdgePipelineSlot::PerShader(id) => CompileInstallTarget::EdgeResolvePerShader {
                    shader_id: id.shader_id,
                    mipmaps: id.mipmaps,
                },
                EdgePipelineSlot::Skybox => CompileInstallTarget::EdgeResolveSkybox,
                EdgePipelineSlot::FinalBlend => CompileInstallTarget::EdgeResolveFinalBlend,
            };
            // Layout-level, not owned by any material: charge to the single
            // non-material MaterialEdgeResolve group. `apply_compile_
            // resolution_inline` validates edge installs by layout-content
            // (`is_edge_key_desired`), so `id` / `generation` are not
            // consulted for edge targets — they're carried only for logging.
            let id = PipelineGroupId::Pass(PassKind::MaterialEdgeResolve);
            let generation = 0u32;
            let fut: std::pin::Pin<
                Box<dyn std::future::Future<Output = PipelineCompileResolution>>,
            > = Box::pin(async move {
                let result: std::result::Result<web_sys::GpuComputePipeline, JsValue> =
                    promise.await;
                PipelineCompileResolution {
                    id,
                    generation,
                    target,
                    cache_key,
                    result,
                }
            });
            self.pipeline_scheduler.push_compile_future_no_count(fut);
        }
        tracing::info!(
            target: "awsm_renderer::pipeline_readiness",
            "launch_edge_resolve_compile: {} layout-level edge sub-pipelines pushed",
            promise_jobs_count,
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

        // Edge-resolve pipelines are LAYOUT-level: they're charged to no
        // material, and their install validity is by layout-content, not by
        // any material's generation. Handle them before the material-id /
        // waiter machinery. Install iff the resolved key is still one the
        // CURRENT layout wants (`is_edge_key_desired`) — otherwise the
        // bucket layout moved on while this compiled, so drop it. (`id` is
        // `PassKind::MaterialEdgeResolve`, `generation` is unused here.)
        match &target {
            CompileInstallTarget::EdgeResolvePerShader { .. }
            | CompileInstallTarget::EdgeResolveSkybox
            | CompileInstallTarget::EdgeResolveFinalBlend => {
                self.render_passes
                    .material_opaque
                    .edge_pipelines
                    .clear_edge_key_in_flight(&cache_key);
                if !self
                    .render_passes
                    .material_opaque
                    .edge_pipelines
                    .is_edge_key_desired(&cache_key)
                {
                    return;
                }
                match result {
                    Ok(pipeline) => {
                        let pipeline_key = self
                            .pipelines
                            .compute
                            .install_resolved_pipeline(pipeline, cache_key);
                        install_edge_per_pass(self, &edge_slot_from_target(&target), pipeline_key);
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "awsm_renderer::pipeline_readiness",
                            "apply_compile_resolution: edge pipeline compile failed: {:?}", e
                        );
                    }
                }
                return;
            }
            CompileInstallTarget::ClassifyDynamic { .. }
            | CompileInstallTarget::OpaqueDynamic { .. } => {}
        }

        // Take the full waiter list — every material that registered
        // an interest in this cache key (the original launcher + every
        // later launch that hit `register_compute_compile_waiter`'s
        // cross-call dedup path). Each waiter's subcompile counter
        // was bumped at register-time; we must decrement here, even
        // on stale-drop / failure, otherwise late-waiter materials
        // would stay Pending forever.
        let waiters = self
            .pipeline_scheduler
            .take_compute_compile_waiters(&cache_key);

        let mid = match id {
            PipelineGroupId::Material(mid) => mid,
            PipelineGroupId::Pass(_) => {
                tracing::warn!(
                    target: "awsm_renderer::pipeline_readiness",
                    "apply_compile_resolution: Pass-flavoured resolution not yet supported"
                );
                // Balance waiter counts even on this unsupported
                // path so materials don't get stuck Pending.
                for waiter_mid in waiters {
                    self.pipeline_scheduler.note_subcompile_complete(waiter_mid);
                }
                return;
            }
        };

        // Stale-generation drop: a config flip / bucket-grow /
        // texture-pool-grow bumped this material's generation between
        // promise-issuance and resolution. The compiled pipeline was
        // built against an outdated cache key — discard the install.
        //
        // Subtle: this check uses the PROMISE's launcher generation,
        // not each waiter's. That's correct — every waiter on this
        // cache_key wanted THE SAME pipeline (cache keys embed every
        // input that affects compile output). If the original
        // launcher's gen is stale, every waiter's wait on this key
        // is stale too — the bucket-grow / config-flip path would
        // have triggered fresh launches for the new cache keys for
        // each of them, and those will resolve and install
        // separately.
        let stale = self
            .pipeline_scheduler
            .material_generation(mid)
            .map(|g| g != generation)
            .unwrap_or(true);

        if stale {
            tracing::debug!(
                target: "awsm_renderer::pipeline_readiness",
                "apply_compile_resolution: stale resolution dropped (material({:?}), waiters={})",
                mid,
                waiters.len()
            );
            for waiter_mid in waiters {
                self.pipeline_scheduler.note_subcompile_complete(waiter_mid);
            }
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
                // Drain waiter counts so additional waiters don't get
                // stuck Pending. They'll still be marked Failed if
                // any of their OWN compiles fail; this cache_key's
                // failure only marks the primary launcher Failed
                // (matching the existing single-waiter contract —
                // late waiters may be on the same broken shader, but
                // they'll surface their own failure events
                // separately).
                for waiter_mid in waiters {
                    self.pipeline_scheduler.note_subcompile_complete(waiter_mid);
                }
                return;
            }
        };

        // Install: slotmap + cache + per-pass. Only opaque/classify targets
        // reach here — edge-resolve targets are handled by the layout-level
        // early-return branch at the top of this fn. The per-pass install is
        // GLOBAL (keyed by shader_id / msaa / mipmaps), so the single
        // install covers every waiter material.
        let pipeline_key = self
            .pipelines
            .compute
            .install_resolved_pipeline(pipeline, cache_key);
        match &target {
            CompileInstallTarget::ClassifyDynamic { .. }
            | CompileInstallTarget::OpaqueDynamic { .. } => {
                install_per_pass(
                    self,
                    &launch_slot_from_target(&target),
                    shader_id_from_target(&target),
                    dispatch_hash_from_target(&target),
                    pipeline_key,
                );
            }
            CompileInstallTarget::EdgeResolvePerShader { .. }
            | CompileInstallTarget::EdgeResolveSkybox
            | CompileInstallTarget::EdgeResolveFinalBlend => {
                unreachable!("edge-resolve targets are installed by the layout-level branch")
            }
        }

        // Decrement every waiter's subcompile counter. Materials
        // whose counter hits zero transition Pending → Ready and
        // emit a status event.
        for waiter_mid in waiters {
            self.pipeline_scheduler.note_subcompile_complete(waiter_mid);
        }
    }
}

/// Resolve a first-party bucket's `(base, pbr_features, owns_skybox)` for
/// the opaque cache key from its registry bucket entry. A per-feature-set
/// PBR/Toon variant reports its specialized family + feature mask; the
/// canonical first-party buckets carry the EMPTY feature-set (the canonical
/// PBR bucket is the skybox owner — it shades no material geometry, so it
/// compiles the minimal shader, never an "uber" all-features one). Only the
/// canonical `MaterialShaderId::PBR` bucket owns the skybox write (classify
/// routes skybox pixels to bit 0 / index 0 → that bucket), so every
/// specialized PBR variant gets `owns_skybox = false`.
fn opaque_variant_params(
    entries: &[crate::dynamic_materials::BucketEntry],
    shader_id: MaterialShaderId,
) -> (crate::dynamic_materials::ShadingBase, u32, bool) {
    let entry = entries.iter().find(|e| e.shader_id == shader_id);
    let base = entry
        .map(|e| e.base)
        .unwrap_or_else(|| crate::dynamic_materials::ShadingBase::for_shader_id(shader_id));
    // Missing entry → empty feature-set (the minimal shader), never the
    // full "uber" set. A missing entry is a defensive fallback that
    // shouldn't happen; if it does we'd rather under-shade than silently
    // compile + run the uber path.
    let pbr_features = entry
        .map(|e| e.pbr_features)
        .unwrap_or_else(|| awsm_materials::pbr::PbrFeatures::default().bits());
    let owns_skybox = shader_id == MaterialShaderId::PBR;
    (base, pbr_features, owns_skybox)
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
        CompileInstallTarget::EdgeResolvePerShader { .. }
        | CompileInstallTarget::EdgeResolveSkybox
        | CompileInstallTarget::EdgeResolveFinalBlend => {
            unreachable!(
                "launch_slot_from_target: edge variants route through edge_slot_from_target"
            )
        }
    }
}

fn shader_id_from_target(t: &CompileInstallTarget) -> MaterialShaderId {
    match t {
        CompileInstallTarget::OpaqueDynamic { shader_id, .. } => *shader_id,
        // Classify target doesn't carry shader_id; sentinel value
        // (the install path doesn't read it for classify).
        CompileInstallTarget::ClassifyDynamic { .. } => MaterialShaderId::PBR,
        CompileInstallTarget::EdgeResolvePerShader { .. }
        | CompileInstallTarget::EdgeResolveSkybox
        | CompileInstallTarget::EdgeResolveFinalBlend => {
            unreachable!("shader_id_from_target: edge variants route through edge_slot_from_target")
        }
    }
}

fn dispatch_hash_from_target(t: &CompileInstallTarget) -> u64 {
    match t {
        CompileInstallTarget::ClassifyDynamic { dispatch_hash, .. } => *dispatch_hash,
        CompileInstallTarget::OpaqueDynamic { .. } => 0,
        CompileInstallTarget::EdgeResolvePerShader { .. }
        | CompileInstallTarget::EdgeResolveSkybox
        | CompileInstallTarget::EdgeResolveFinalBlend => {
            unreachable!(
                "dispatch_hash_from_target: edge variants route through edge_slot_from_target"
            )
        }
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

/// Wrapper around an [`EdgePipelineSlot`] so the edge-launch path
/// can shuttle slot identities around without conflicting with the
/// opaque/classify [`LaunchSlot`] enum above. Pure marshalling — the
/// install path destructures back to `EdgePipelineSlot` immediately.
#[derive(Clone)]
struct EdgeLaunchSlot(EdgePipelineSlot);

fn install_edge_per_pass(
    renderer: &mut crate::AwsmRenderer,
    slot: &EdgePipelineSlot,
    pipeline_key: crate::pipelines::compute_pipeline::ComputePipelineKey,
) {
    let edge = &mut renderer.render_passes.material_opaque.edge_pipelines;
    match slot {
        EdgePipelineSlot::PerShader(id) => {
            edge.insert_per_shader_pipeline(*id, pipeline_key);
        }
        EdgePipelineSlot::Skybox => {
            edge.skybox_edge_resolve_pipeline_key = Some(pipeline_key);
        }
        EdgePipelineSlot::FinalBlend => {
            edge.final_blend_pipeline_key = Some(pipeline_key);
        }
    }
}

/// Reconstruct the [`EdgePipelineSlot`] identity from a
/// [`CompileInstallTarget`] for the apply-resolution path. Only the
/// three `EdgeResolve*` variants are valid input; other variants
/// would panic.
fn edge_slot_from_target(t: &CompileInstallTarget) -> EdgePipelineSlot {
    match t {
        CompileInstallTarget::EdgeResolvePerShader { shader_id, mipmaps } => {
            EdgePipelineSlot::PerShader(EdgeResolvePipelineKeyId {
                shader_id: *shader_id,
                mipmaps: *mipmaps,
            })
        }
        CompileInstallTarget::EdgeResolveSkybox => EdgePipelineSlot::Skybox,
        CompileInstallTarget::EdgeResolveFinalBlend => EdgePipelineSlot::FinalBlend,
        _ => unreachable!("edge_slot_from_target called with non-edge variant"),
    }
}
