//! Render-driven, family-agnostic, AA-agnostic pipeline-compile path.
//!
//! There is ONE compile-launch operation:
//! [`AwsmRenderer::ensure_scene_pipelines`]. It is SYNC + non-blocking
//! (the render loop can't await — it kicks async compiles and the
//! dispatch sites skip not-ready pipelines until they resolve). It
//! compiles exactly what the LIVE scene needs at the ACTIVE AA config,
//! via the lazy content-keyed pipeline caches underneath.
//!
//! Flow:
//!
//! 1. **Dirty-gated.** The render preamble calls it every frame; on the
//!    warm path it is a single `bool` check (`materials.variants_dirty`)
//!    and returns immediately — zero per-frame iteration / allocation.
//! 2. **Bucket-layout change first.** When the live bucket SET changed
//!    (`dispatch_hash` or entry count drifted from the last ensure), it
//!    resizes the classify + edge GPU buffers, rebuilds the edge-layout
//!    uniform, and CLEARS the stale layout-keyed pipeline caches BEFORE
//!    compiling against the new layout. Getting this ordering wrong =
//!    pipelines dispatch against mismatched classify-output offsets =
//!    corrupted shading, so it is strictly ordered.
//! 3. **Classify + per-bucket opaque.** For the ACTIVE `(msaa, mipmaps)`
//!    only (never the 4× msaa×mipmap blowup), it ensures the classify
//!    pipeline and every opaque-routed bucket's opaque pipeline via the
//!    shared content-keyed cache, charging each compile to the bucket's
//!    scheduler `Material(mid)` group (so a CUSTOM material's WGSL
//!    compile error surfaces via `dynamic_material_compile_status`).
//! 4. **Edge resolve.** Delegates to the layout-level
//!    [`AwsmRenderer::launch_edge_resolve_compile`] (idempotent,
//!    dedup'd by cache + desired-keys).
//!
//! `AwsmRenderer::poll_pipeline_scheduler` drains the scheduler's
//! `inflight_compile` queue per-frame and `apply_compile_resolution`
//! installs the resolved pipeline into the per-pass cache + decrements
//! the owning material's sub-compile counter. When the last sub-pipeline
//! lands, the material flips Pending → Ready and frontends subscribed to
//! the status stream observe the transition.

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

/// Pull the WGSL compile diagnostic for a failed pipeline compile by routing
/// through the shader module's `getCompilationInfo`. The `createComputePipeline
/// Async` rejection value is typically an opaque `GPUPipelineError` whose
/// message is empty / non-actionable; `getCompilationInfo` is where the real
/// line/column + message live. Awaited inside the compile future (alongside the
/// pipeline promise) so the sync apply site has the text ready.
///
/// Returns `None` when no module was captured (edge pipelines), when the info
/// fetch itself fails, or when the module reports no errors — the apply site
/// then falls back to the raw rejection value.
async fn shader_compile_diagnostic(module: Option<web_sys::GpuShaderModule>) -> Option<String> {
    use awsm_renderer_core::shaders::ShaderModuleExt;
    let info = module?.get_compilation_info_ext().await.ok()?;
    if info.errors.is_empty() {
        return None;
    }
    let mut out = String::new();
    for msg in &info.errors {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "line {}:{}: {}",
            msg.line_num, msg.line_pos, msg.message
        ));
    }
    Some(out)
}

impl crate::AwsmRenderer {
    /// THE render-driven, family-agnostic, AA-agnostic compile-launch
    /// operation. Compiles on demand exactly what the LIVE scene needs
    /// at the ACTIVE AA config, via the lazy content-keyed pipeline
    /// caches underneath. SYNC + non-blocking — it kicks async compiles
    /// and returns; the dispatch sites skip not-ready pipelines until
    /// they resolve via `poll_pipeline_scheduler`.
    ///
    /// Called from the render preamble. It is gated by the caller on
    /// `materials.variants_dirty` (see `reconcile_material_variants`),
    /// so the warm/unchanged path never reaches here — zero per-frame
    /// iteration / allocation.
    ///
    /// Steps (strictly ordered):
    ///
    /// 1. Capture `active_msaa` / `active_mipmaps`.
    /// 2. **Bucket-layout change.** If the live bucket SET changed
    ///    (`dispatch_hash` or entry count drifted from
    ///    `last_ensured_bucket_layout`), resize the classify + edge GPU
    ///    buffers, rebuild the edge-layout uniform, and CLEAR the stale
    ///    layout-keyed pipeline caches BEFORE compiling against the new
    ///    layout — getting this ordering wrong dispatches pipelines
    ///    against mismatched classify-output offsets.
    /// 3. **Classify** (active msaa) — ensured + installed.
    /// 4. **Per-bucket opaque** (active msaa × mipmaps) — every
    ///    opaque-routed bucket. A CUSTOM material's compile is charged
    ///    to its `Material(mid)` group so a WGSL error surfaces via
    ///    `dynamic_material_compile_status`; first-party variants charge
    ///    to their own (never-failing) group the same way.
    /// 5. **Edge resolve** (active_msaa MSAA + supported) — delegated to
    ///    the layout-level `launch_edge_resolve_compile`.
    ///
    /// Every bucket has a scheduler `Material(mid)` group: this method
    /// submits one for any bucket lacking it (so its compile is
    /// chargeable + observable). The shared per-bucket compile reuses
    /// the same cross-call waiter dedup the old launch paths used, so
    /// the classify compile (keyed on `(msaa, bucket_entries)`, shared
    /// by every bucket) is issued once and every bucket waits on it.
    pub fn ensure_scene_pipelines(&mut self) -> Result<(), crate::error::AwsmError> {
        let active_msaa = self.anti_aliasing.msaa_sample_count;
        let active_mipmaps = self.anti_aliasing.mipmap;

        // ── Step 1: detect a bucket-SET change and, if so, re-lay-out the
        //    bucket-dependent GPU buffers + clear the stale layout-keyed
        //    pipeline caches BEFORE compiling against the new layout.
        let dispatch_hash = self.dynamic_materials.dispatch_hash_cached();
        let bucket_count = self.dynamic_materials.bucket_entries_cached().len();
        let layout_signature = (dispatch_hash, bucket_count);
        let layout_changed = self.last_ensured_bucket_layout != Some(layout_signature);
        if layout_changed {
            self.relayout_bucket_buffers(bucket_count as u32)?;
            self.last_ensured_bucket_layout = Some(layout_signature);
        }

        // ── Step 2: ensure a scheduler entry for every live bucket so each
        //    bucket's compile is chargeable + observable. Idempotent — skips
        //    buckets that already have an entry. Snapshot the ids first to
        //    avoid borrowing the registry across the mutating submit.
        let bucket_ids: Vec<MaterialShaderId> = self
            .dynamic_materials
            .bucket_entries_cached()
            .iter()
            .map(|e| e.shader_id)
            .collect();
        for id in &bucket_ids {
            if let Err(e) = self.submit_to_scheduler_for_shader_id(*id) {
                tracing::warn!(
                    target: "awsm_renderer::pipeline_readiness",
                    "ensure_scene_pipelines: submit_to_scheduler({:?}) failed: {:?}", id, e
                );
            }
        }

        // ── On a bucket-SET change, re-mark every still-tracked material
        //    `Pending` + bump its generation. The generation bump is what
        //    makes `apply_compile_resolution`'s stale-generation gate DROP
        //    any in-flight resolution compiled against the PREVIOUS layout
        //    (the caches were just cleared in `relayout_bucket_buffers`;
        //    without the bump a late old-layout promise would install a
        //    pipeline that dispatches against the new classify offsets).
        //    It also keeps the compile-status surface honest — existing
        //    materials show Pending while their replacement compiles.
        if layout_changed {
            for id in &bucket_ids {
                if let Some(mid) = self.pipeline_scheduler.find_material_by_shader_id(*id) {
                    self.pipeline_scheduler
                        .mark_material_pending_for_relaunch(PipelineGroupId::Material(mid));
                }
            }
        }

        // ── Step 3: classify + per-bucket opaque, active config only.
        //    The classify cache key is shared across buckets (keyed on
        //    `(msaa, bucket_entries)`, not shader_id); the per-bucket
        //    compile helper's cross-call waiter dedup issues it once (on
        //    the first bucket, always the SKYBOX bucket at index 0) and
        //    makes every later bucket wait on it. So classify lands even on
        //    a Blend-only custom scene whose own opaque compile is skipped.
        let entries = self.dynamic_materials.bucket_entries_cached().to_vec();

        for shader_id in &bucket_ids {
            if let Err(e) = self.ensure_bucket_pipelines(
                *shader_id,
                &entries,
                dispatch_hash,
                active_msaa,
                active_mipmaps,
            ) {
                tracing::warn!(
                    target: "awsm_renderer::pipeline_readiness",
                    "ensure_scene_pipelines: bucket({:?}) compile failed: {:?}",
                    shader_id, e
                );
            }
        }

        // ── Step 4: edge resolve — layout-level, idempotent (deduped by
        //    cache + in-flight + desired_keys). No-op when MSAA is off or
        //    `edge_resolve_supported` is false.
        self.launch_edge_resolve_compile()?;

        Ok(())
    }

    /// Re-lay-out the bucket-dependent GPU state for `bucket_count`:
    /// classify buffers (per-bucket indirect args + tile lists), the
    /// edge buffers + edge-layout uniform (MSAA only), then CLEAR the
    /// layout-keyed pipeline caches (opaque `main` + the edge per-shader
    /// + globals).
    ///
    /// **Ordering is load-bearing.** Resize the buffers and clear the
    /// caches BEFORE [`Self::ensure_scene_pipelines`] compiles against
    /// the new layout. Were the old (smaller-bucket) pipelines left in
    /// the layout-keyed caches (their lookup keys are bucket-layout-
    /// AGNOSTIC — `(shader_id, msaa, mipmaps)` for opaque), dispatch in
    /// the window before the new compiles land would read them against
    /// the freshly-resized classify / edge buffers, where every
    /// `<shader>_offset` field has shifted — corrupted shading, worse
    /// than a skipped draw. After the clear the dispatch site's `Option`
    /// guard returns `None` and skips the draw until the new pipeline
    /// lands. The classify per-pass cache is self-invalidating (keyed on
    /// `dispatch_hash`, which changes on every bucket mutation) so old
    /// entries become unreachable orphans — no clear needed there.
    fn relayout_bucket_buffers(
        &mut self,
        bucket_count: u32,
    ) -> Result<(), crate::error::AwsmError> {
        // Classify buffers (per-bucket indirect args + tile lists). A
        // realloc invalidates every bind group that referenced the old
        // buffer — mark them for recreation.
        if self
            .material_classify_buffers
            .ensure_bucket_count(&self.gpu, bucket_count)?
        {
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::MaterialClassifyBuffersResize);
        }
        // Rebuild + upload the `shader_id → bucket_index` LUT (§4a) for the
        // new bucket set. This is the only site the LUT changes (the bucket
        // set just changed); a buffer-grow forces the classify bind group to
        // rebind. Done here — not in `ClassifyBuffers` — because the LUT must
        // survive viewport-resize reallocs of the classify buckets.
        if self
            .material_bucket_lut
            .ensure(&self.gpu, self.dynamic_materials.bucket_entries_cached())?
        {
            self.bind_groups
                .mark_create(crate::bind_groups::BindGroupCreate::MaterialClassifyBuffersResize);
        }
        // Edge buffers + edge-layout uniform (MSAA only). The edge
        // args/data buffers live on the classify multi-sampled bind group
        // (binding 4/5) — the classify recreate mark covers them too.
        if let Some(edge_buffers) = self.material_edge_buffers.as_mut() {
            if edge_buffers.ensure_bucket_count(&self.gpu, bucket_count)? {
                self.bind_groups.mark_create(
                    crate::bind_groups::BindGroupCreate::MaterialClassifyBuffersResize,
                );
                let max_edge_budget = edge_buffers.max_edge_budget;
                if let Ok((uniform, _bytes)) =
                    crate::render_passes::material_opaque::edge_buffers::build_edge_layout_uniform(
                        &self.gpu,
                        bucket_count,
                        max_edge_budget,
                    )
                {
                    self.material_edge_layout_uniform = Some(uniform);
                }
            }
        }

        // Clear the layout-keyed typed pipeline caches (opaque `main` + edge +
        // classify) AND free the GPU pipelines they were holding — the
        // pipeline-leak fix ("aw snap" crash). Each cache holds
        // `ComputePipelineKey`s into the shared `self.pipelines.compute` pool; a
        // bucket-set change makes them stale, so they're cleared here before the
        // new layout compiles. Historically `clear_dynamic_pipelines` only
        // dropped these KEYS — the `GpuComputePipeline` objects lingered in the
        // pool forever (the classify/edge keys are self-invalidating, minting a
        // fresh pool entry on every registry change and orphaning the old), which
        // accumulated unbounded under editing churn → GPU OOM. Now the clears
        // RETURN the dropped keys and we free exactly those from the pool (plus
        // the shader modules they were built from).
        //
        // Freeing precisely the just-dropped keys is dangle-free by construction:
        // nothing references them anymore (the typed caches are now empty and the
        // per-frame `collect_renderables` rebuilds from them, so the dispatch
        // sites' `Option` guard skips the draw until the new layout's pipelines
        // land).
        let mut dropped_keys = self
            .render_passes
            .material_opaque
            .pipelines
            .clear_dynamic_pipelines();
        dropped_keys.extend(
            self.render_passes
                .material_opaque
                .edge_pipelines
                .clear_dynamic_pipelines(),
        );
        // The classify pass's `pipeline_cache` (keyed by `(dispatch_hash, msaa)`,
        // populated by the async install path) accumulates a fresh entry on every
        // registry edit (each mints a new dispatch_hash) and never drops the old
        // ones, so prune everything that isn't the live dispatch_hash and free
        // those pool pipelines too.
        let current_dispatch_hash = self.dynamic_materials.dispatch_hash_cached();
        dropped_keys.extend(
            self.render_passes
                .material_classify
                .prune_dynamic_pipeline_cache(current_dispatch_hash),
        );
        let freed_shaders = self.pipelines.compute.remove_pipeline_keys(&dropped_keys);
        for shader_key in &freed_shaders {
            self.shaders.remove(*shader_key);
        }

        // The clears above free only what the typed caches still REFERENCE. Rapid
        // churn also strands DETACHED orphans in the pool — pipelines created via
        // `ensure_keys` whose resolution was later dropped (stale generation) or
        // whose typed-cache slot was replaced before this clear ran. They're no
        // longer reachable through any typed cache, so the only handle on them is
        // their shader: every set-specialized pipeline (opaque / edge / classify /
        // skybox-edge / final-blend) is built from a shader whose cache key
        // carries the now-stale dispatch_hash / bucket set. Sweep the shader cache
        // for those stale modules and free them + every pool pipeline built from
        // them. This is dangle-free now that the typed caches were cleared/pruned
        // above: no live cache (nor the per-frame renderables) references a
        // stale-signature pipeline anymore. Together the two passes return the
        // pool to baseline under unbounded editing churn (the "aw snap" fix).
        let current_bucket_entries = self.dynamic_materials.bucket_entries_cached().to_vec();
        let stale_shaders = self
            .shaders
            .take_stale_dynamic_set_shader_keys(current_dispatch_hash, &current_bucket_entries);
        if !stale_shaders.is_empty() {
            self.pipelines.compute.remove_by_shader_keys(&stale_shaders);
        }

        Ok(())
    }

    /// Ensure the classify (active msaa) + opaque (active msaa × mipmaps)
    /// pipelines for a single bucket `shader_id`, charging the compile to
    /// the bucket's scheduler `Material(mid)` group.
    ///
    /// Shared by every bucket in [`Self::ensure_scene_pipelines`]. The
    /// classify cache key is identical across buckets (keyed on
    /// `(msaa, bucket_entries, emit_edge_data)`, not shader_id), so the
    /// cross-call waiter dedup here issues its compile once and registers
    /// later buckets as waiters — every bucket's Ready transition still
    /// waits on the shared classify compile.
    ///
    /// A CUSTOM (registered) material attaches its `DynamicShaderInfo`
    /// (its WGSL is templated into the opaque kernel); a first-party
    /// canonical bucket / feature-set variant compiles the built-in body
    /// (`dynamic_shader: None`). A Blend/Mask custom registration skips
    /// the opaque compile (its body targets the transparent contract).
    ///
    /// Charging the compile to `Material(mid)` is what surfaces a custom
    /// material's WGSL compile error via
    /// `dynamic_material_compile_status` (the `create_compute_pipeline_async`
    /// rejection flows to `apply_compile_resolution` → `mark_failed`).
    fn ensure_bucket_pipelines(
        &mut self,
        shader_id: MaterialShaderId,
        entries: &[crate::dynamic_materials::BucketEntry],
        dispatch_hash: u64,
        active_msaa: Option<u32>,
        active_mipmaps: bool,
    ) -> Result<(), crate::error::AwsmError> {
        let Some(mid) = self
            .pipeline_scheduler
            .find_material_by_shader_id(shader_id)
        else {
            return Ok(());
        };
        let group_id = PipelineGroupId::Material(mid);
        let Some(generation) = self.pipeline_scheduler.material_generation(mid) else {
            return Ok(());
        };

        // Orphan guard: registration was removed but the scheduler group
        // lingers — nothing to compile. Mark Ready so a stray entry can't
        // hang the compile-status surface. (Canonical first-party + live
        // custom + known fp-variant ids are all launchable.)
        if !self.is_launchable_material(shader_id) {
            self.pipeline_scheduler.mark_ready(group_id);
            return Ok(());
        }

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

        // `reg` is `Some` only for a CUSTOM author material; a first-party
        // feature-set variant has a dynamic-range id but no registration.
        let reg = self.dynamic_materials.get(shader_id).cloned();
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
        let (base, pbr_features, owns_skybox) = opaque_variant_params(entries, shader_id);

        let mut shader_jobs: Vec<crate::shaders::ShaderCacheKey> = Vec::new();
        let mut slots: Vec<LaunchSlot> = Vec::new();

        // Classify variant for the ACTIVE MSAA only (the only one
        // dispatched). Shared cache key across buckets.
        shader_jobs.push(
            ShaderCacheKeyMaterialClassify {
                msaa_sample_count: active_msaa,
                bucket_count: entries.len() as u32,
                emit_edge_data: active_msaa.is_some() && crate::edge_resolve_supported(&self.gpu),
            }
            .into(),
        );
        slots.push(LaunchSlot::Classify { msaa: active_msaa });

        // Opaque variant for THIS bucket — built when it routes to the opaque
        // pass: OPAQUE and (now) MASK customs. A MASK custom is alpha-tested
        // opaque — its MAIN WGSL shades in the opaque compute (OpaqueShadingOutput
        // contract) while its 2nd alpha-only WGSL discards cutouts in the masked
        // visibility raster. Only BLEND targets the transparent contract.
        // First-party variants (no registration) always build.
        let build_opaque = reg.as_ref().is_none_or(|r| {
            matches!(
                r.alpha_mode,
                MaterialAlphaMode::Opaque | MaterialAlphaMode::Mask { .. }
            )
        });
        if build_opaque {
            shader_jobs.push(
                ShaderCacheKeyMaterialOpaque {
                    texture_pool_arrays_len,
                    texture_pool_samplers_len,
                    msaa_sample_count: active_msaa,
                    mipmaps: active_mipmaps,
                    prep_enabled: self.prep_config.enabled,
                    max_shadow_casters: self.prep_config.clamped_k(),
                    shader_id,
                    base,
                    owns_skybox,
                    pbr_features,
                    dispatch_hash,
                    dynamic_shader: dynamic_shader.clone(),
                    bucket_entries: entries.to_vec(),
                }
                .into(),
            );
            slots.push(LaunchSlot::Opaque {
                msaa: active_msaa,
                mipmaps: active_mipmaps,
            });
        }

        // Sync shader install — skips validation; validation errors
        // re-surface as pipeline-creation rejections (→ `mark_failed`).
        let resolved_shader_keys = self
            .shaders
            .ensure_keys_sync_skip_validate(&self.gpu, shader_jobs)?;

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
            // § Part B (the "1024 fix"): the opaque module now exposes TWO
            // `@compute` entry points (`cs_opaque` + `cs_edge`), so the opaque
            // pipeline must name `cs_opaque` explicitly. Classify keeps its
            // single default entry point.
            let cache_key = match &slot {
                LaunchSlot::Opaque { .. } => {
                    ComputePipelineCacheKey::new(shader_key, layout).with_entry_point("cs_opaque")
                }
                LaunchSlot::Classify { .. } => ComputePipelineCacheKey::new(shader_key, layout),
            };
            compute_jobs.push((slot, cache_key));
        }

        // Cache-hit + cross-call waiter dedup. The classify cache key is
        // shared by every bucket in this ensure pass: the first bucket
        // issues the promise; later buckets register as waiters (so their
        // Ready transition waits on the shared compile) and skip the
        // duplicate `createComputePipelineAsync`. Waiter registration is
        // deferred until after the fallible `ensure_keys_prepare` succeeds
        // so a prep error never leaks counter / waiter-map state.
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

        let promise_count = promise_jobs.len();

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
                // Clone the shader module so the future can pull a real WGSL
                // diagnostic (via `getCompilationInfo`) if the compile rejects.
                let shader_module = self.shaders.get(cache_key.shader_key).cloned();
                let fut: std::pin::Pin<
                    Box<dyn std::future::Future<Output = PipelineCompileResolution>>,
                > = Box::pin(async move {
                    let result: std::result::Result<web_sys::GpuComputePipeline, JsValue> =
                        promise.await;
                    let compile_error = if result.is_err() {
                        shader_compile_diagnostic(shader_module).await
                    } else {
                        None
                    };
                    PipelineCompileResolution {
                        id,
                        generation,
                        target,
                        cache_key,
                        result,
                        compile_error,
                    }
                });
                self.pipeline_scheduler.push_compile_future_no_count(fut);
            }
            tracing::info!(
                target: "awsm_renderer::pipeline_readiness",
                "ensure_bucket_pipelines({:?}): {} sub-pipelines pushed for material({:?})",
                shader_id,
                promise_count,
                mid,
            );
        }

        // Every sub-pipeline was a cache hit (or already in flight as a
        // skip — those decrement via their resolution): if this material
        // has no pending subcompiles, mark Ready inline.
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
                self.prep_config.enabled,
                self.prep_config.clamped_k(),
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

        // Build compute pipeline cache keys per entry. § Part B: per-shader
        // edge pipelines select the `cs_edge` entry point on the UNIFIED
        // opaque shader module (`descs.entry_points`); skybox/final_blend
        // keep their single default entry point.
        let mut compute_jobs: Vec<(EdgeLaunchSlot, ComputePipelineCacheKey)> =
            Vec::with_capacity(descs.slots.len());
        for (((shader_key, layout_key), entry_point), slot) in resolved_shader_keys
            .into_iter()
            .zip(descs.pipeline_layout_keys.iter().copied())
            .zip(descs.entry_points.iter().cloned())
            .zip(descs.slots.iter().copied())
        {
            let edge_slot = EdgeLaunchSlot(slot);
            let cache_key = match entry_point {
                Some(name) => {
                    ComputePipelineCacheKey::new(shader_key, layout_key).with_entry_point(&name)
                }
                None => ComputePipelineCacheKey::new(shader_key, layout_key),
            };
            compute_jobs.push((edge_slot, cache_key));
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
        let (mut hits, mut inflight_skips) = (0usize, 0usize);
        for (slot, cache_key) in &compute_jobs {
            if let Some(existing_key) = self.pipelines.compute.cache_lookup(cache_key).copied() {
                install_edge_per_pass(self, &slot.0, existing_key);
                hits += 1;
            } else if self
                .render_passes
                .material_opaque
                .edge_pipelines
                .edge_key_in_flight(cache_key)
            {
                // A prior launch this window already has this compile in
                // flight; its single resolution installs for the layout.
                inflight_skips += 1;
                tracing::debug!(
                    target: "awsm_renderer::pipeline_readiness",
                    "launch_edge_resolve_compile: {:?} skipped as in-flight",
                    slot.0,
                );
            } else {
                promise_jobs.push((slot.clone(), cache_key.clone()));
            }
        }

        if promise_jobs.is_empty() {
            // Every pipeline was a cache hit or already in flight. Logged
            // because a stuck in-flight marker here looks like "settled but
            // frozen" downstream — this line is the breadcrumb.
            tracing::debug!(
                target: "awsm_renderer::pipeline_readiness",
                "launch_edge_resolve_compile: 0 pushed ({} cache-hit installs, {} in-flight skips)",
                hits,
                inflight_skips,
            );
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
                    // Edge pipelines are layout-level (not owned by any
                    // material); their failures are logged, not surfaced to an
                    // author via compile-status, so no diagnostic is pulled.
                    compile_error: None,
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
            compile_error,
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
                    // The layout moved on while this compiled. Logged because a
                    // dropped FinalBlend with no follow-up relaunch presents as
                    // the frozen-canvas mode (preamble warn-skip, settled:true).
                    tracing::debug!(
                        target: "awsm_renderer::pipeline_readiness",
                        "apply_compile_resolution: edge resolution no longer desired — dropped (slot {:?})",
                        edge_slot_from_target(&target),
                    );
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
                // Prefer the real WGSL diagnostic (line/column + message, via
                // `getCompilationInfo`) the compile future resolved alongside
                // the rejection; fall back to the raw rejection value when the
                // module reported no errors (e.g. a layout / binding mismatch
                // rather than a shader syntax/type error).
                let detail =
                    compile_error.unwrap_or_else(|| format!("{e:?} (no shader compilation info)"));
                tracing::warn!(
                    target: "awsm_renderer::pipeline_readiness",
                    "apply_compile_resolution: pipeline-creation failed for material({:?}): {}",
                    mid,
                    detail,
                );
                self.pipeline_scheduler
                    .mark_failed(id, crate::error::AwsmError::MaterialShaderCompile(detail));
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
/// canonical first-party buckets carry the EMPTY feature-set (the minimal
/// shader, never an "uber" all-features one). Only the dedicated
/// `MaterialShaderId::SKYBOX` bucket (index 0) owns the skybox write — classify
/// routes every uncovered pixel to bit 0, and its pipeline is the
/// `skybox_primary` writer (it shades no geometry); every material bucket gets
/// `owns_skybox = false`.
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
    let owns_skybox = shader_id == MaterialShaderId::SKYBOX;
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

/// True when `shader_id` is still a live bucket in the current registered set
/// (first-party ids are always present). A resolution for a shader_id that's NOT
/// live — a dynamic material deleted between compile-launch and resolution —
/// must NOT be installed into the per-bucket typed caches: it would strand a
/// stale `(…, shader_id)` entry that the bucket-set clear can't reach (the clear
/// re-fills only live buckets), leaking it permanently. Part of the pipeline-leak
/// fix.
fn is_live_bucket(renderer: &crate::AwsmRenderer, shader_id: MaterialShaderId) -> bool {
    renderer
        .dynamic_materials
        .bucket_entries_cached()
        .iter()
        .any(|e| e.shader_id == shader_id)
}

fn install_per_pass(
    renderer: &mut crate::AwsmRenderer,
    slot: &LaunchSlot,
    shader_id: MaterialShaderId,
    dispatch_hash: u64,
    pipeline_key: crate::pipelines::compute_pipeline::ComputePipelineKey,
) {
    use crate::render_passes::material_opaque::pipeline::PipelineKeyId;
    // Drop a late opaque resolution for a now-deleted bucket (see is_live_bucket).
    // Classify is shared infra keyed on a sentinel (always-live) shader_id, so it
    // is exempt — only the per-bucket opaque install is guarded.
    if matches!(slot, LaunchSlot::Opaque { .. }) && !is_live_bucket(renderer, shader_id) {
        free_displaced_compute_pipeline(renderer, pipeline_key);
        return;
    }
    // Free the pool pipeline displaced by this install, if any — re-installing a
    // typed-cache slot under a new bucket layout used to silently orphan the
    // previous `GpuComputePipeline` (part of the pipeline-leak fix). The
    // displaced key is no longer referenced by anything, so freeing it is safe.
    let displaced = match slot {
        LaunchSlot::Classify { msaa } => renderer
            .render_passes
            .material_classify
            .pipeline_cache
            .borrow_mut()
            .insert((dispatch_hash, *msaa), pipeline_key)
            .filter(|old| *old != pipeline_key),
        LaunchSlot::Opaque { msaa, mipmaps } => renderer
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
            ),
    };
    if let Some(old) = displaced {
        free_displaced_compute_pipeline(renderer, old);
    }
}

/// Free a single displaced compute pipeline (and its shader module) from the
/// shared pool — used by the install sites when a typed-cache slot is
/// overwritten under a new bucket layout. Part of the pipeline-leak fix.
fn free_displaced_compute_pipeline(
    renderer: &mut crate::AwsmRenderer,
    key: crate::pipelines::compute_pipeline::ComputePipelineKey,
) {
    let freed_shaders = renderer.pipelines.compute.remove_pipeline_keys(&[key]);
    for shader_key in &freed_shaders {
        renderer.shaders.remove(*shader_key);
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
    // Drop a late per-shader edge resolution for a now-deleted bucket — same
    // rationale as install_per_pass. Skybox / final-blend are global (no
    // per-bucket key) so they're always installed.
    if let EdgePipelineSlot::PerShader(id) = slot {
        if !is_live_bucket(renderer, id.shader_id) {
            free_displaced_compute_pipeline(renderer, pipeline_key);
            return;
        }
    }
    let edge = &mut renderer.render_passes.material_opaque.edge_pipelines;
    // Capture any pool key displaced by this install so we can free the orphaned
    // pipeline (part of the pipeline-leak fix — see install_per_pass).
    let displaced = match slot {
        EdgePipelineSlot::PerShader(id) => edge.insert_per_shader_pipeline(*id, pipeline_key),
        EdgePipelineSlot::Skybox => edge
            .skybox_edge_resolve_pipeline_key
            .replace(pipeline_key)
            .filter(|old| *old != pipeline_key),
        EdgePipelineSlot::FinalBlend => edge
            .final_blend_pipeline_key
            .replace(pipeline_key)
            .filter(|old| *old != pipeline_key),
    };
    if let Some(old) = displaced {
        free_displaced_compute_pipeline(renderer, old);
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
