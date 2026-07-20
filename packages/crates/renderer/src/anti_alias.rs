//! Anti-aliasing configuration.

use crate::{bind_groups::BindGroupCreate, error::Result, AwsmRenderer};

/// Anti-aliasing configuration for the renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AntiAliasing {
    // if None, no MSAA
    // Some(4) is the only supported option for now
    pub msaa_sample_count: Option<u32>,
    pub smaa: bool,
    pub mipmap: bool,
}

impl AntiAliasing {
    /// Returns whether MSAA is enabled and supported.
    pub fn has_msaa_checked(&self) -> crate::error::Result<bool> {
        match self.msaa_sample_count {
            Some(4) => Ok(true),
            None => Ok(false),
            Some(sample_count) => Err(crate::error::AwsmError::UnsupportedMsaaCount(sample_count)),
        }
    }
}

impl Default for AntiAliasing {
    fn default() -> Self {
        Self {
            // Some(4) is the only supported option for now
            msaa_sample_count: Some(4),
            //msaa_sample_count: None,
            // OFF by default. On-device A/B on real content (thin HDR emissive
            // lines over 4x MSAA) showed marginal gains at best: SMAA was
            // designed as an MSAA *replacement* for renderers that couldn't
            // afford MSAA, and layered on top of an MSAA resolve it has little
            // left to fix — while on 1–2px bright lines its revectorization
            // redistributes line energy visibly. Off is zero-cost (no pass, no
            // textures, no shader variant); the toggle remains for A/B and for
            // MSAA-less configurations, where it earns its keep.
            smaa: false,
            mipmap: true,
        }
    }
}

impl AwsmRenderer {
    /// Updates the anti-aliasing settings and recompiles the
    /// AA-dependent pipelines for the new config.
    ///
    /// **Two-tier model:**
    /// - **Material pipelines** (classify / opaque / per-shader edge
    ///   resolve): this method only FLAGS the reconcile
    ///   (`mark_variants_dirty`) + resets the ensure fingerprint — it does
    ///   NOT recompile them. An AA change is a config change that needs
    ///   recompilation, so the caller must follow `set_anti_aliasing` with
    ///   `commit_load` (the one compile path); `commit_load`'s
    ///   `reconcile_material_variants` → `ensure_scene_pipelines` then
    ///   recompiles exactly the new config's `(msaa, mipmaps)` variant.
    ///   (Pre-load-transaction this happened reactively in the render
    ///   preamble; that preamble is gone.)
    /// - **Non-material passes** (geometry / HZB / picker / transparent /
    ///   effects / lines / shadows) have no scene-compile path, so their
    ///   MSAA-variant recompiles stay here, awaited up front.
    ///
    /// Already-compiled variants from previous MSAA states stay cached,
    /// so toggling back-and-forth pays the compile cost only on the first
    /// transition in each direction.
    pub async fn set_anti_aliasing(&mut self, aa: AntiAliasing) -> Result<()> {
        // Race policy per https://github.com/dakom/awsm-renderer/pull/99: config-change
        // APIs return NotReady when called before build() finishes its
        // eager batch. The frontends already structure their renderer
        // lifecycle to call this post-`build().await`; this just makes
        // the contract explicit.
        if !self.build_complete {
            return Err(crate::error::AwsmError::NotReady);
        }
        // No-op fast path — caller asked for the state we're
        // already in. The bind-group recreate marks are skipped too;
        // there's nothing for them to invalidate.
        if self.anti_aliasing == aa {
            return Ok(());
        }
        // Deferred-boot: drain any still-reserved boot-pool slots first, so
        // the branch guards below (`has_branch_for`) reflect COMPILED reality
        // — a reserved-but-pending branch must not be mistaken for a ready
        // one. No-op after the first commit_load / ensure_config_pipelines.
        self.compile_pending_pipelines().await?;
        let prev_msaa_on = self.anti_aliasing.has_msaa_checked()?;
        self.anti_aliasing = aa;
        let new_msaa_on = self.anti_aliasing.has_msaa_checked()?;

        // ── SMAA lifecycle: build the pre-pass on enable, DROP it on disable
        //    (textures destroyed — SMAA off is zero-cost; the effects pass's
        //    smaa-off shader variant contains no SMAA code and binds a 1×1
        //    dummy weights texture). Enable-side construction mirrors bloom's
        //    lazy `set_post_processing` build.
        if self.anti_aliasing.smaa && self.render_passes.smaa.is_none() {
            let mut ctx = crate::render_passes::RenderPassInitContext {
                gpu: &self.gpu,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                render_texture_formats: &mut self.render_textures.formats,
                textures: &mut self.textures,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self
                    .material_edge_buffers
                    .as_ref()
                    .map(|b| b.max_edge_budget)
                    .unwrap_or(
                        crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP,
                    ),
            };
            let smaa = crate::render_passes::smaa::render_pass::SmaaRenderPass::new(&mut ctx, 1, 1)
                .await?;
            self.render_passes.smaa = Some(smaa);
        } else if !self.anti_aliasing.smaa {
            if let Some(smaa) = self.render_passes.smaa.take() {
                smaa.textures.destroy();
            }
        }

        // MSAA off → on transition: allocate `material_edge_buffers`
        // + `material_edge_layout_uniform` if they're not already
        // resident. The multisampled classify bind-group layout
        // statically includes the edge bindings (slots 4..=9) at
        // boot, but the buffers themselves are only allocated when
        // MSAA is on at build time (see `lib.rs`'s `edge_resolve_enabled`
        // gate). Without this allocation, the next frame's
        // `BindGroupCreate::AntiAliasingChange`-driven recreate of
        // the multisampled classify bind group has nothing to bind
        // into slots 4..=9 → WebGPU rejects the bind-group create
        // with a "required entry missing" validation error.
        //
        // Gated on `edge_resolve_supported` (matches the build-site's
        // `edge_resolve_enabled`). MSAA-on transition on a device
        // that doesn't support the full edge_resolve dispatch wiring
        // is a no-op here — the multisampled classify layout was
        // built without slots 4..=9 to match (see
        // `create_bind_group_layout_key`'s `edge_emit_supported` gate),
        // so the base 4-entry bind group is valid.
        if !prev_msaa_on
            && new_msaa_on
            && self.material_edge_buffers.is_none()
            && crate::edge_resolve_supported(&self.gpu)
        {
            let bucket_count = self.dynamic_materials.bucket_entries_cached().len() as u32;
            use crate::render_passes::material_opaque::edge_buffers::{
                build_edge_layout_uniform, MaterialEdgeBuffers,
            };
            let edge_buffers = MaterialEdgeBuffers::new(
                &self.gpu,
                bucket_count,
                self.post_processing.ssr.enabled,
            )?;
            let max_edge_budget = edge_buffers.max_edge_budget;
            let (uniform, _bytes) =
                build_edge_layout_uniform(&self.gpu, bucket_count, max_edge_budget)?;
            self.material_edge_buffers = Some(edge_buffers);
            self.material_edge_layout_uniform = Some(uniform);
        }

        // MSAA on → off transition: tear the edge buffers back down, mirroring
        // the off → on allocation above. This restores the invariant the rest of
        // the renderer assumes — `material_edge_buffers.is_some()` ⟺ MSAA-on —
        // which the classify bind group keys off (its multisampled layout
        // statically declares the edge slots; see `material_classify::bind_group`)
        // and which the per-frame edge bookkeeping (`reset_header`, the
        // overflow-readback copy + mapAsync in `render`) uses to skip work. Before
        // this, those buffers leaked across the flip and ran wasted per-frame work
        // every MSAA-off frame — and left the multisampled-only `cs_prep_edge` /
        // `cs_shade` edge bindings able to bind a single-sampled main group.
        //
        // Safe to drop synchronously: `MaterialEdgeBuffers` has no `Drop` and
        // never calls `.destroy()`, so this just releases the `web_sys` GPU-buffer
        // handles to GC. WebGPU keeps the underlying memory alive for any in-flight
        // submit, so (unlike `RenderTextures`, which destroys eagerly) no
        // deferred-destroy is needed. Gated only on `!new_msaa_on`: on devices
        // without `edge_resolve_supported` the buffers are already `None`, so the
        // clear is an idempotent no-op.
        if !new_msaa_on {
            self.material_edge_buffers = None;
            self.material_edge_layout_uniform = None;
        }

        // The prep pass's compact edge-shadow texture (~8 MB) follows the
        // same MSAA lifecycle: allocated on the off→on flip (the opaque MSAA
        // main bind group binds its view at binding 27 — unconditionally
        // under MSAA, independent of `edge_resolve_supported`), dropped on
        // on→off. The `TextureViewRecreate` mark below re-clones the view
        // into the dependent bind groups next frame.
        if let Some(prep) = self.render_passes.material_prep.as_mut() {
            if new_msaa_on {
                let max_edge_budget = self
                    .material_edge_buffers
                    .as_ref()
                    .map(|b| b.max_edge_budget)
                    .unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP);
                prep.ensure_edge_shadow(
                    &self.gpu,
                    max_edge_budget,
                    self.prep_config.shadow_visibility_layers(),
                )?;
            } else {
                prep.drop_edge_shadow();
            }
        }

        self.bind_groups
            .mark_create(BindGroupCreate::AntiAliasingChange);
        self.bind_groups
            .mark_create(BindGroupCreate::TextureViewRecreate);

        // Material pipelines (classify / opaque / per-shader edge resolve):
        // flag the reconcile + reset the ensure fingerprint so the caller's
        // follow-up `commit_load` (the one compile path) recompiles the active
        // `(msaa, mipmaps)` variant for every live bucket against the new config
        // (clearing the stale layout-keyed caches + bumping generations to drop
        // in-flight old-config resolutions). render() no longer drives this —
        // `set_anti_aliasing` MUST be followed by `commit_load`.
        self.last_ensured_bucket_layout = None;
        self.materials.mark_variants_dirty();

        // ── Non-material compute passes (HZB + picker): no render-driven
        //    re-ensure exists for these, so their MSAA-variant recompiles
        //    stay here. Each builder returns the cache keys for the NEW
        //    config; already-compiled variants resolve as cache hits.

        // HZB descriptors — only present when `features.gpu_culling`
        // is on.
        let hzb_descs = if let Some(hzb) = self.render_passes.hzb.as_ref() {
            Some(
                crate::render_passes::hzb::pipeline::HzbPipelines::build_descriptors_for_config(
                    &self.gpu,
                    &mut self.bind_group_layouts,
                    &mut self.pipeline_layouts,
                    &mut self.shaders,
                    &hzb.bind_groups,
                    &self.anti_aliasing,
                    self.features.reverse_z,
                )
                .await?,
            )
        } else {
            None
        };

        // Picker descriptors — only present when the picker has been
        // lazily compiled (Block B.4: `self.picker` stays `None` until
        // first `pick()` query even when `features.picking == true`).
        // When the picker isn't yet built, this block is skipped — the
        // next `pick()` compiles it for the live AA config. When it IS
        // built, returns the picker's BGLs + the (single) pipeline cache
        // key for the new MSAA; the previously-compiled variant on
        // `self.picker` is preserved via `merge_resolved`.
        let picker_descs = if let Some(picker) = self.picker.as_ref() {
            let _ = picker; // bind for clarity; we only need to know it's Some
            Some(
                crate::picker::Picker::build_descriptors(
                    &self.gpu,
                    &mut self.bind_group_layouts,
                    &mut self.pipeline_layouts,
                    &mut self.shaders,
                    &self.anti_aliasing,
                )
                .await?,
            )
        } else {
            None
        };

        // Batched compute compile for the HZB + picker keys (union).
        use crate::pipelines::compute_pipeline::ComputePipelineCacheKey;
        let mut compute_jobs: Vec<ComputePipelineCacheKey> = Vec::new();
        let hzb_range = hzb_descs.as_ref().map(|d| {
            let start = compute_jobs.len();
            compute_jobs.extend(d.pipeline_cache_keys.iter().cloned());
            start..compute_jobs.len()
        });
        let picker_range = picker_descs.as_ref().map(|d| {
            let start = compute_jobs.len();
            compute_jobs.extend(d.pipeline_cache_keys.iter().cloned());
            start..compute_jobs.len()
        });

        let resolved = if compute_jobs.is_empty() {
            Vec::new()
        } else {
            self.pipelines
                .compute
                .ensure_keys(
                    &self.gpu,
                    &self.shaders,
                    &self.pipeline_layouts,
                    compute_jobs,
                )
                .await?
        };

        // Merge resolved pipelines into the per-pass caches (sync slotmap
        // inserts; previously-compiled variants preserved).
        if let (Some(hzb_descs), Some(hzb_range), Some(hzb_pass)) =
            (hzb_descs, hzb_range, self.render_passes.hzb.as_mut())
        {
            hzb_pass
                .pipelines
                .merge_resolved(hzb_descs.slot, resolved[hzb_range].to_vec());
        }
        if let (Some(picker_descs), Some(picker_range), Some(picker)) =
            (picker_descs, picker_range, self.picker.as_mut())
        {
            picker.merge_resolved(picker_descs.slot, resolved[picker_range].to_vec());
        }

        // ── Phase 4b: geometry MSAA branch recompile (lazy-pool).
        //    Skip when the new MSAA's branch is already populated
        //    (the user previously toggled this way and back). When
        //    new, build 9 render pipelines + 3 shaders for just the
        //    new branch and fold into the existing nested struct.
        let multisampled_geometry = self.anti_aliasing.has_msaa_checked()?;
        if !self
            .render_passes
            .geometry
            .pipelines
            .has_branch_for(&self.anti_aliasing)
        {
            // Phase 4b.i: shader compile batch for the new branch's 3 keys.
            let geometry_shader_keys_needed =
                crate::render_passes::geometry::pipeline::GeometryPipelines::shader_cache_keys(
                    multisampled_geometry,
                );
            self.shaders
                .ensure_keys(&self.gpu, geometry_shader_keys_needed)
                .await?;

            // Phase 4b.ii: build the new branch's 9 render pipeline
            // descriptors. Reuses the same RenderPassInitContext
            // shape the cold-boot path uses.
            let mut ctx = crate::render_passes::RenderPassInitContext {
                gpu: &self.gpu,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                render_texture_formats: &mut self.render_textures.formats,
                textures: &mut self.textures,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            let geometry_descs =
                crate::render_passes::geometry::pipeline::GeometryPipelines::build_descriptors(
                    &mut ctx,
                    &self.render_passes.geometry.bind_groups,
                    multisampled_geometry,
                )
                .await?;

            // Phase 4b.iii: batch render pipeline compile.
            let geometry_pipeline_keys = self
                .pipelines
                .render
                .ensure_keys(
                    &self.gpu,
                    &self.shaders,
                    &self.pipeline_layouts,
                    geometry_descs.pipeline_cache_keys.clone(),
                )
                .await?;

            // Phase 4b.iv: merge into the existing struct (preserves
            // any previously-populated MSAA branch).
            self.render_passes
                .geometry
                .pipelines
                .merge_resolved(&geometry_descs, geometry_pipeline_keys)?;
        }

        // ── Phase 4c: material-prep MSAA branch recompile (lazy-pool,
        //    mirrors Phase 4b). Boot compiled only the then-active branch;
        //    fill the incoming branch's cs_prep (+ cs_prep_edge under MSAA on
        //    a supporting device, + the blur pair while denoise is on) if any
        //    of it is missing. Cache-keyed, so a flip-back — or a redundant
        //    piece within the branch — resolves as a hit.
        let prep_edge_resolve = new_msaa_on && crate::edge_resolve_supported(&self.gpu);
        let prep_denoise = self.prep_config.denoise;
        let prep_branch_missing = self.render_passes.material_prep.as_ref().is_some_and(|p| {
            !p.pipelines
                .has_branch_for(new_msaa_on, prep_edge_resolve, prep_denoise)
        });
        if prep_branch_missing {
            use crate::render_passes::material_prep::render_pass::MaterialPrepPipelines;
            // Phase 4c.i: shader batch for the branch (megashader module +
            // blur module). Warm already when only e.g. the blur pair is
            // missing — ensure_keys is cache-keyed.
            self.shaders
                .ensure_keys(
                    &self.gpu,
                    MaterialPrepPipelines::shader_cache_keys(
                        new_msaa_on,
                        &self.prep_config,
                        self.features.reverse_z,
                    ),
                )
                .await?;

            // Phase 4c.ii: build the branch's pipeline cache keys. Same
            // RenderPassInitContext shape as Phase 4b; `render_passes` is not
            // part of the ctx, so borrowing the prep bind groups alongside is
            // disjoint.
            let mut ctx = crate::render_passes::RenderPassInitContext {
                gpu: &self.gpu,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                render_texture_formats: &mut self.render_textures.formats,
                textures: &mut self.textures,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            let prep = self
                .render_passes
                .material_prep
                .as_ref()
                .expect("checked is_some_and above");
            let prep_descs = MaterialPrepPipelines::build_descriptors_for_config(
                &mut ctx,
                &prep.bind_groups,
                new_msaa_on,
                prep_edge_resolve,
            )
            .await?;

            // Phase 4c.iii: batched compute compile + merge (preserves the
            // other branch's already-resolved keys).
            let prep_keys = self
                .pipelines
                .compute
                .ensure_keys(
                    &self.gpu,
                    &self.shaders,
                    &self.pipeline_layouts,
                    prep_descs.pipeline_cache_keys.clone(),
                )
                .await?;
            if let Some(prep) = self.render_passes.material_prep.as_mut() {
                prep.pipelines.merge_resolved(&prep_descs.slots, prep_keys);
            }
        }

        // ── Phase 5: transparent pipelines depend on per-mesh
        //    attributes AND AA settings — recompile every live
        //    mesh's variant. Batched inside `set_render_pipeline_keys_batched`.
        let mut requests: Vec<
            crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest,
        > = Vec::new();
        for (mesh_key, mesh) in self.meshes.iter() {
            // Only transparent-pass meshes get a transparent pipeline — an
            // opaque (incl. opaque-dynamic) material can't compile against the
            // transparent fragment contract.
            if !self.materials.is_transparency_pass(mesh.material_key) {
                continue;
            }
            let buffer_info_key = self.meshes.buffer_info_key(mesh_key)?;
            let writes_depth = self.materials.transparent_writes_depth(mesh.material_key);
            let (base, pbr_features) = self.materials.transparent_variant(mesh.material_key);
            let dynamic_shader_id = matches!(base, crate::dynamic_materials::ShadingBase::Custom)
                .then(|| self.materials.shader_id(mesh.material_key));
            let dynamic_shader =
                dynamic_shader_id.and_then(|id| self.dynamic_materials.shader_info_for(id));
            let dynamic_vertex_shader =
                dynamic_shader_id.and_then(|id| self.dynamic_materials.vertex_shader_info_for(id));
            requests.push(
                crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest {
                    mesh,
                    mesh_key,
                    buffer_info_key,
                    writes_depth,
                    base,
                    pbr_features,
                    dynamic_shader_id,
                    dynamic_shader,
                    dynamic_vertex_shader,
                },
            );
        }
        self.render_passes
            .material_transparent
            .pipelines
            .set_render_pipeline_keys_batched(
                &self.gpu,
                requests,
                &mut self.shaders,
                &mut self.pipelines,
                &self.render_passes.material_transparent.bind_groups,
                &self.pipeline_layouts,
                &self.meshes.buffer_infos,
                &self.anti_aliasing,
                &self.textures,
                &self.render_textures.formats,
                self.features.depth().compare(),
                self.features.reverse_z,
            )
            .await?;

        // ── Phase 6: effects pass (post-processing) — its own
        //    batched ensure inside `set_render_pipeline_keys`.
        self.render_passes
            .effects
            .pipelines
            .set_render_pipeline_keys(
                &self.anti_aliasing,
                &self.post_processing,
                &self.gpu,
                &mut self.shaders,
                &mut self.pipelines,
                &self.pipeline_layouts,
                &self.render_textures.formats,
                self.features.reverse_z,
            )
            .await?;

        // NOTE: the MSAA edge-resolve pipeline set is layout-level (its cache
        // keys embed the whole bucket list); the `mark_variants_dirty` flag set
        // above makes the caller's follow-up `commit_load` →
        // `ensure_scene_pipelines` → `launch_edge_resolve_compile` rebuild it
        // for the new config. `multisampled_geometry` (computed in Phase 4b for
        // the geometry branch's lazy-pool selector) equals `new_msaa_on`; assert
        // that invariant once for documentation.
        debug_assert_eq!(new_msaa_on, multisampled_geometry);

        // ── Phase 8 (Block B.3): line pipelines lazy ensure. Cold-boot
        //    leaves the 4 line-pipeline variants uncompiled; the first
        //    `add_line_*` flips `pipelines_compile_requested`. Recompile
        //    on MSAA flip is a no-op once variants populate.
        if !self.lines.is_empty() || self.lines.pipelines_compile_requested() {
            self.ensure_line_pipelines_compiled().await?;
        }

        // ── Phase 9 (Block B.1 + B.2): shadow pipeline compile. Caster
        //    + EVSM pipelines are MSAA-invariant (depth-only fragment +
        //    compute) so a flip doesn't itself require recompile, but
        //    use this `.await` as a convenient moment to land any
        //    pending compile if a shadow caster is registered and
        //    pipelines aren't yet compiled. No-op when nothing to do.
        self.ensure_shadow_pipelines_compiled().await?;

        // ── Phase 10: SSR pass rebuild. The SSR trace + composite bake the
        //    DEPTH binding's `multisampled` flag into their bind-group
        //    LAYOUTS at construction (`ctx.anti_aliasing`), so an MSAA flip
        //    leaves them binding the wrong depth-texture sample count —
        //    invalid bind group → the whole frame's command buffer fails
        //    (black screen; found by the 004 verification matrix). Rebuild
        //    exactly like `set_post_processing` does for the structural SSR
        //    axes. LAZY SSR (axis 1): skip entirely while the pass is `None`
        //    (never enabled) — the eventual first enable constructs it
        //    against the then-live AA, so nothing can go stale.
        if self.render_passes.ssr.is_some() {
            let mut ctx = crate::render_passes::RenderPassInitContext {
                gpu: &self.gpu,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                render_texture_formats: &mut self.render_textures.formats,
                textures: &mut self.textures,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self
                    .material_edge_buffers
                    .as_ref()
                    .map(|b| b.max_edge_budget)
                    .unwrap_or(
                        crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP,
                    ),
            };
            let ssr = crate::render_passes::ssr::render_pass::SsrRenderPass::new(&mut ctx).await?;
            self.render_passes.ssr = Some(ssr);
        }

        Ok(())
    }
}

impl AwsmRenderer {
    /// Current supersampling factor (1.0 = off).
    pub fn render_scale(&self) -> f32 {
        self.render_scale
    }

    /// True when supersampling is active (drives the display shader's
    /// downsample variant).
    pub(crate) fn render_scale_supersamples(&self) -> bool {
        (self.render_scale - 1.0).abs() > f32::EPSILON
    }

    /// The internal render resolution: swap-chain size scaled by
    /// [`Self::render_scale`] (identical to the swap-chain size at 1.0).
    /// Every viewport-derived consumer (render targets, light-culling
    /// froxels, LOD screen-error, camera viewport uniform, picker) reads
    /// THIS, never the raw swap-chain size — the display pass output and
    /// swap-chain readbacks are the only canvas-sized stages.
    pub(crate) fn render_size(&self) -> Result<(u32, u32)> {
        let (w, h) = self.gpu.current_context_texture_size()?;
        Ok((
            crate::size::scale_extent(w, self.render_scale),
            crate::size::scale_extent(h, self.render_scale),
        ))
    }

    /// Runtime anisotropic-filtering toggle (default ON). OFF rebinds every
    /// anisotropic material sampler to its no-aniso twin — pool indices and
    /// material data untouched, so the flip is one texture-pool bind-group
    /// rebuild (next frame), no pipeline work.
    pub fn set_anisotropy_enabled(&mut self, on: bool) {
        if self.textures.set_anisotropy_enabled(on) {
            self.bind_groups.mark_create(BindGroupCreate::TexturePool);
        }
    }

    pub fn anisotropy_enabled(&self) -> bool {
        self.textures.anisotropy_enabled()
    }

    /// Sets the supersampling factor (clamped to [1.0, 2.0]).
    ///
    /// Optional quality setting shared by players and the editor — OFF
    /// (1.0) by default; 2.0 renders 4x the pixels and box-downsamples,
    /// which is the honest fix for sub-pixel/thin-feature crawl that
    /// post-process AA cannot reconstruct. Render targets rebuild lazily
    /// on the next frame (the size change flows through
    /// `RenderTextures::views` + the bind-group ledger); only the display
    /// pipeline (1:1 load vs downsample variant) recompiles here.
    pub async fn set_render_scale(&mut self, scale: f32) -> Result<()> {
        let scale = scale.clamp(1.0, 2.0);
        if (scale - self.render_scale).abs() <= f32::EPSILON {
            return Ok(());
        }
        let was_supersampling = self.render_scale_supersamples();
        self.render_scale = scale;
        if self.render_scale_supersamples() != was_supersampling {
            self.render_passes
                .display
                .pipelines
                .set_render_pipeline_key(
                    &self.post_processing,
                    self.render_scale_supersamples(),
                    &self.gpu,
                    &mut self.shaders,
                    &mut self.pipelines,
                    &self.pipeline_layouts,
                    &self.render_textures.formats,
                )
                .await?;
        }
        Ok(())
    }
}
