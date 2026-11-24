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
    /// Temporal anti-aliasing (and temporal upscaling when its
    /// `render_scale` is below 1). `Some` requires `msaa_sample_count: None`
    /// and `smaa: false`: the three are independent modes, and
    /// [`Self::validate`] rejects a combination rather than picking one
    /// silently. `None` costs nothing: no targets, buffers, copies, passes
    /// or shader work.
    pub taa: Option<crate::taa::TaaConfig>,
}

impl AntiAliasing {
    /// No anti-aliasing at all.
    pub fn none() -> Self {
        Self {
            msaa_sample_count: None,
            smaa: false,
            mipmap: true,
            taa: None,
        }
    }

    /// MSAA 4×.
    pub fn msaa4() -> Self {
        Self {
            msaa_sample_count: Some(4),
            ..Self::none()
        }
    }

    /// SMAA (post-process, no MSAA).
    pub fn smaa() -> Self {
        Self {
            smaa: true,
            ..Self::none()
        }
    }

    /// TAA with `config` (MSAA and SMAA off).
    pub fn taa(config: crate::taa::TaaConfig) -> Self {
        Self {
            taa: Some(config),
            ..Self::none()
        }
    }

    /// Checks the mode is one the renderer supports: a supported MSAA count,
    /// and TAA neither combined with MSAA or SMAA nor given a render scale
    /// outside `0.5..=1.0`.
    pub fn validate(&self) -> crate::error::Result<()> {
        self.has_msaa_checked()?;
        if let Some(taa) = &self.taa {
            if self.msaa_sample_count.is_some() || self.smaa {
                return Err(crate::error::AwsmError::TaaCombinedWithOtherAa);
            }
            let scale = taa.render_scale;
            if !(scale.is_finite()
                && (crate::taa::TaaConfig::MIN_RENDER_SCALE..=1.0).contains(&scale))
            {
                return Err(crate::error::AwsmError::TaaRenderScale(scale));
            }
        }
        Ok(())
    }

    /// Whether `self` and `other` need the same targets, pipelines and passes:
    /// equal except possibly inside `taa: Some(..)` (its tuning, sharpening
    /// and render scale are live values). Render textures rebuild, and
    /// `set_anti_aliasing` recompiles, only when this is false.
    pub fn same_structure(&self, other: &AntiAliasing) -> bool {
        self.msaa_sample_count == other.msaa_sample_count
            && self.smaa == other.smaa
            && self.mipmap == other.mipmap
            && self.taa.is_some() == other.taa.is_some()
    }

    /// Returns whether MSAA is enabled and supported.
    pub fn has_msaa_checked(&self) -> crate::error::Result<bool> {
        match self.msaa_sample_count {
            Some(4) => Ok(true),
            None => Ok(false),
            Some(sample_count) => Err(crate::error::AwsmError::UnsupportedMsaaCount(sample_count)),
        }
    }
}

/// Bytes per sample of the geometry targets with TAA's motion-vector target:
/// 4 × 8 (visibility, barycentric, normal/tangent, derivatives) + 4
/// (`rg16float` velocity).
pub const TAA_COLOR_ATTACHMENT_BYTES_PER_SAMPLE: u32 = 36;

/// Storage buffers visible to the vertex stage in the largest TAA geometry
/// variant (masked + custom-vertex): materials, material meta, the geometry
/// pool, texture transforms (group 0), transforms + the shared previous-frame
/// buffer (group 1), morph weights + values, joint matrices + joint weights
/// (group 3). Chrome on Apple GPUs allows exactly 10, which is why the
/// previous transforms, joint matrices and morph weights share one buffer.
pub const TAA_GEOMETRY_STORAGE_BUFFERS: u32 = 10;

/// Where TAA's motion vectors come from on this device
/// ([`AwsmRenderer::taa_velocity_path`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TaaVelocityPath {
    /// A fifth geometry-pass target, written per surface with the previous
    /// frame's vertex evaluation (03 §2.2). Exact for every kind of motion.
    #[default]
    GeometryTarget,
    /// Reconstructed in the prep pass from depth and the previous object
    /// matrix (03 §2.4): devices at 32 bytes per sample. Phase 5.
    PrepReconstruction,
}

/// The motion-vector path this device supports (see
/// [`AwsmRenderer::taa_velocity_path`]).
pub(crate) fn velocity_path_for_device(
    gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
) -> TaaVelocityPath {
    let limits = gpu.device.limits();
    if limits.max_color_attachment_bytes_per_sample() >= TAA_COLOR_ATTACHMENT_BYTES_PER_SAMPLE
        && limits.max_storage_buffers_per_shader_stage() >= TAA_GEOMETRY_STORAGE_BUFFERS
    {
        TaaVelocityPath::GeometryTarget
    } else {
        TaaVelocityPath::PrepReconstruction
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
            taa: None,
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
    ///
    /// TAA that cannot be built on this device (its pass failed to create)
    /// leaves the renderer on MSAA 4x, with the reason in
    /// [`Self::taa_fallback_reason`], and returns the error. (A TAA pipeline
    /// that fails later, in the commit's compile, falls back the same way
    /// there.)
    pub async fn set_anti_aliasing(&mut self, aa: AntiAliasing) -> Result<()> {
        let result = self.set_anti_aliasing_inner(aa).await;
        // Failed half-way into TAA: the config says TAA but there is no TAA
        // pass to resolve with. Do not leave the renderer like that.
        if result.is_err() && self.anti_aliasing.taa.is_some() && self.render_passes.taa.is_none() {
            if let Err(err) = &result {
                self.taa.fallback_reason = Some(err.to_string());
                tracing::warn!("TAA unavailable, falling back to MSAA 4x: {err}");
            }
            let fallback = AntiAliasing {
                mipmap: self.anti_aliasing.mipmap,
                ..AntiAliasing::msaa4()
            };
            Box::pin(self.set_anti_aliasing_inner(fallback)).await?;
        }
        result
    }

    async fn set_anti_aliasing_inner(&mut self, aa: AntiAliasing) -> Result<()> {
        // Race policy per https://github.com/dakom/awsm-renderer/pull/99: config-change
        // APIs return NotReady when called before build() finishes its
        // eager batch. The frontends already structure their renderer
        // lifecycle to call this post-`build().await`; this just makes
        // the contract explicit.
        if !self.build_complete {
            return Err(crate::error::AwsmError::NotReady);
        }
        // The motion-vector path is renderer state (the device's, or the
        // debug override), not part of the config: a change of it (the
        // override) is structural like an AA-mode change (03 §2.4).
        let prep_velocity_path = self.taa_velocity_path() == TaaVelocityPath::PrepReconstruction;
        let path_changed = prep_velocity_path != self.render_textures.formats.prep_velocity_path;
        // No-op fast path — caller asked for the state we're
        // already in. The bind-group recreate marks are skipped too;
        // there's nothing for them to invalidate.
        if self.anti_aliasing == aa && !path_changed {
            return Ok(());
        }
        aa.validate()?;
        if aa.taa.is_some() {
            // Supersampling (`set_render_scale` > 1) with TAA is not supported
            // in v1 (plan 04, "keep out of v1").
            if self.render_scale_supersamples() {
                return Err(crate::error::AwsmError::TaaCombinedWithOtherAa);
            }
        }
        // TAA stays on and only its config changed (tuning, sharpening, render
        // scale): nothing structural moves. No pipeline, texture or bind-group
        // rebuild and NO history reset (03 §8: a render-scale change is not a
        // reset); the resolve's params and the frame sizes follow the new
        // config on the next frame.
        if self.anti_aliasing.same_structure(&aa) && !path_changed {
            self.anti_aliasing = aa;
            // Sharpening on <-> off builds / drops the RCAS pass (its own
            // pipeline only; the frame's pipelines are untouched).
            self.sync_taa_rcas().await?;
            return Ok(());
        }
        // On a committed scene: drain anything still reserved first, so the
        // branch guards below (`has_branch_for`) reflect COMPILED reality — a
        // reserved-but-pending branch must not be mistaken for a ready one.
        // (Normally a no-op: a commit leaves nothing reserved.)
        //
        // Not while the pools are deferred (before the first commit, or after
        // `begin_load`): nothing draws yet, everything this setter builds is
        // reserved too, and the commit compiles the lot as one batch. Draining
        // here would bring back a compile wave of its own for an embedder
        // that sets AA while loading — and would make a post-process pipeline
        // the driver rejects fail THIS call, when the commit's drain turns
        // that effect off and carries on.
        if !self.pipelines.is_deferred() {
            self.compile_pending_pipelines().await?.into_result()?;
        }
        let prev_msaa_on = self.anti_aliasing.has_msaa_checked()?;
        let prev_geometry_targets =
            crate::render_passes::geometry::pipeline::GeometryTargets::from_aa(
                &self.anti_aliasing,
                &self.render_textures.formats,
            );
        self.anti_aliasing = aa;
        self.render_textures.formats.prep_velocity_path = prep_velocity_path;
        let new_msaa_on = self.anti_aliasing.has_msaa_checked()?;
        let new_geometry_targets =
            crate::render_passes::geometry::pipeline::GeometryTargets::from_aa(
                &self.anti_aliasing,
                &self.render_textures.formats,
            );
        // TAA's previous-frame buffers exist only while TAA is on.
        self.sync_motion_history();

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

        // ── TAA lifecycle: build the resolve pass (shader, pipeline,
        //    textures) on enable and reset the temporal history, DROP it on
        //    disable (textures destroyed — TAA off is zero-cost). Same shape
        //    as SMAA above.
        self.sync_taa_pass().await?;

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
            .has_branch_for(&self.anti_aliasing, &self.render_textures.formats)
        {
            // Phase 4b.i: shader compile batch for the new branch's 3 keys.
            let geometry_shader_keys_needed =
                crate::render_passes::geometry::pipeline::GeometryPipelines::shader_cache_keys(
                    new_geometry_targets,
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
                    new_geometry_targets,
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

        // ── Phase 4b″: the masked / custom-vertex / combined geometry pools
        //    build their TAA layout only while TAA is on (their variants for
        //    the new targets compile at the follow-up commit, like an MSAA
        //    flip's).
        {
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
            let geometry = &mut self.render_passes.geometry;
            geometry.masked_pipelines.sync_velocity_layout(
                &mut ctx,
                &geometry.masked_bind_group,
                &geometry.bind_groups,
            )?;
            geometry.custom_vertex_pipelines.sync_velocity_layout(
                &mut ctx,
                &geometry.masked_bind_group,
                &geometry.bind_groups,
            )?;
            geometry
                .masked_custom_vertex_pipelines
                .sync_velocity_layout(
                    &mut ctx,
                    &geometry.masked_bind_group,
                    &geometry.bind_groups,
                )?;
        }

        // ── Phase 4b′: before the first commit, give back the branch being
        //    left. `build()` reserved the geometry pipelines and the HZB seed
        //    for the MSAA mode the renderer was built with; an embedder that
        //    changes MSAA while loading (a phone player turning it off) would
        //    otherwise have the commit compile both branches — nine geometry
        //    pipelines and a seed that nothing has drawn with or will. On a
        //    committed scene both branches are kept, so a toggle back is a
        //    cache hit.
        if self.pipelines.is_deferred() && prev_geometry_targets != new_geometry_targets {
            let stale = self
                .render_passes
                .geometry
                .pipelines
                .take_branch(prev_geometry_targets);
            self.pipelines.render.release_reserved(stale);
        }
        if self.pipelines.is_deferred() && prev_msaa_on != new_msaa_on {
            if let Some(hzb) = self.render_passes.hzb.as_mut() {
                let seed = if prev_msaa_on {
                    hzb.pipelines.seed_msaa.take()
                } else {
                    hzb.pipelines.seed_single.take()
                };
                self.pipelines.compute.release_reserved(seed);
            }
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
            !p.pipelines.has_branch_for(
                new_msaa_on,
                prep_edge_resolve,
                prep_denoise,
                self.prep_config.clamped_denoise_radius(),
                self.render_textures
                    .formats
                    .taa_prep_velocity(&self.anti_aliasing),
            )
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
                        self.render_textures
                            .formats
                            .taa_prep_velocity(&self.anti_aliasing),
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
                let displaced = prep.pipelines.merge_resolved(&prep_descs, prep_keys);
                self.pipelines.compute.release_reserved(displaced);
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
            // A mesh can outlive its custom material's registration (the
            // material was unregistered while the mesh awaits teardown).
            // Without the registration there is no `MaterialData` struct to
            // emit, and the shader would fail to compile ("unresolved type
            // 'MaterialData'") — failing the whole batch, i.e. every other
            // mesh's pipeline too. Leave that one mesh without a pipeline.
            if dynamic_shader_id.is_some() && dynamic_shader.is_none() {
                continue;
            }
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
            let new_keys = ssr.pipelines.keys();
            if let Some(old) = self.render_passes.ssr.replace(ssr) {
                self.release_replaced_pass_slots(old.pipelines.keys(), &new_keys);
            }
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

    /// Surface, render and post sizes for the live swap chain (see
    /// [`crate::size::FrameSizes`]). The one place the swap-chain size is
    /// turned into the sizes render textures are allocated and dispatched at;
    /// `render()` snaps it once per frame.
    pub(crate) fn frame_sizes(&self) -> Result<crate::size::FrameSizes> {
        let surface = self.gpu.current_context_texture_size()?;
        Ok(crate::size::FrameSizes::new(
            surface,
            self.scene_scale(),
            self.post_is_surface(),
        ))
    }

    /// Scene size / surface size. Without TAA: the supersampling
    /// [`Self::render_scale`] (`[1, 2]`). With TAA: the TAA render scale
    /// (`0.5..=1`, quantised; below 1 the resolve upscales).
    pub(crate) fn scene_scale(&self) -> f32 {
        match &self.anti_aliasing.taa {
            Some(taa) => taa.clamped_render_scale(),
            None => self.render_scale,
        }
    }

    /// Whether the display pass downsamples its input (post size larger than
    /// the surface): supersampling without TAA. Under TAA post size IS the
    /// surface size (the resolve writes the output resolution), so the
    /// display always loads 1:1.
    pub(crate) fn display_downsamples(&self) -> bool {
        self.anti_aliasing.taa.is_none() && self.render_scale_supersamples()
    }

    /// Sets the TAA render scale (`0.5..=1`, quantised to
    /// [`crate::taa::TaaConfig::RENDER_SCALE_STEP`]): the lever for a
    /// resolution governor. Change THIS, never the canvas size: the output
    /// (and the TAA history, bloom and effects) stays at the canvas size, and
    /// a render-scale change is not a history reset (03 §8). Light: no
    /// pipeline compile, no commit. Errors without TAA, or on a non-finite
    /// scale.
    ///
    /// A governor should step it every few seconds at most, with hysteresis
    /// (separate thresholds for lowering and raising, and a hold time after
    /// each step), never per frame: a step is a one-frame hitch. On the next
    /// frame, only when the quantised scale actually changed:
    ///
    /// - Rebuilt at the new size: every render-size target (visibility,
    ///   barycentrics, normals, derivatives, velocity, reactive, depth,
    ///   opaque, transparent, composite, the prep and SSR targets, HUD depth)
    ///   with every bind group that views them, the HZB pyramid, the light
    ///   culling and froxel volume grids. The old textures are destroyed a
    ///   few frames later, so memory briefly holds both sizes. No pipeline
    ///   compiles: sizes are not pipeline state.
    /// - Kept: the TAA history (output size; the previous depth is carried
    ///   across), bloom and the effects targets (output size), LOD and
    ///   coverage LOD (measured in output pixels, so nothing pops), the jitter
    ///   cycle (only its phase count follows the new ratio).
    /// - Restarted: SSR's temporal accumulation (a render-size history;
    ///   re-converges in about ten frames) and the volumetric haze's froxel
    ///   history (its grid follows the render-size light-culling tiles).
    /// - GPU occlusion culling: the HZB is rebuilt from that frame's depth
    ///   before the occlusion test that frame reads it, and the test maps by
    ///   the pyramid's own size, so a step culls exactly like any frame (with
    ///   the usual one frame of latency), never against an empty pyramid.
    pub fn set_taa_render_scale(&mut self, scale: f32) -> Result<()> {
        if !scale.is_finite() {
            return Err(crate::error::AwsmError::TaaRenderScale(scale));
        }
        let Some(taa) = self.anti_aliasing.taa.as_mut() else {
            return Err(crate::error::AwsmError::TaaNotEnabled);
        };
        taa.render_scale = crate::taa::TaaConfig {
            render_scale: scale,
            ..crate::taa::TaaConfig::native()
        }
        .clamped_render_scale();
        Ok(())
    }

    /// The TAA render scale in force (quantised), or `None` without TAA.
    pub fn taa_render_scale(&self) -> Option<f32> {
        self.anti_aliasing
            .taa
            .as_ref()
            .map(|taa| taa.clamped_render_scale())
    }

    /// True when post-processing runs at surface size rather than render size:
    /// with TAA, whose resolve writes the output resolution. Without TAA,
    /// bloom / effects / display input run at render size (supersampling
    /// included, exactly as 0.45).
    fn post_is_surface(&self) -> bool {
        self.anti_aliasing.taa.is_some()
    }

    /// How this device gets TAA motion vectors (03 §2.2, §2.4).
    ///
    /// [`TaaVelocityPath::GeometryTarget`] needs a fifth colour attachment
    /// (`maxColorAttachmentBytesPerSample >= 36`) and the storage buffers of
    /// the TAA geometry variants (current + previous transforms, morph
    /// weights and joint matrices; [`TAA_GEOMETRY_STORAGE_BUFFERS`] in the
    /// largest, masked + custom-vertex, variant). Otherwise the prep pass
    /// reconstructs them ([`TaaVelocityPath::PrepReconstruction`], phase 5).
    /// [`Self::set_taa_velocity_path_override`] forces
    /// the fallback for testing it on a capable device.
    pub fn taa_velocity_path(&self) -> TaaVelocityPath {
        self.taa_velocity_path_override
            .unwrap_or_else(|| velocity_path_for_device(&self.gpu))
    }

    /// Debug / test override for [`Self::taa_velocity_path`] (`None` = by the
    /// device limits). Takes effect at the next `set_anti_aliasing`.
    pub fn set_taa_velocity_path_override(&mut self, path: Option<TaaVelocityPath>) {
        self.taa_velocity_path_override = path;
    }

    /// Turns the previous-frame buffers (transforms, instances, joint
    /// matrices, morph weights) on exactly while TAA is on.
    ///
    /// With the prep-pass fallback (03 §2.4) only what prep reads is kept:
    /// the node transforms. Instances, joint matrices and morph weights get
    /// no previous copy there (prep gives their meshes the rigid motion of
    /// their node and the seed marks them reactive).
    pub(crate) fn sync_motion_history(&mut self) {
        let on = self.anti_aliasing.taa.is_some();
        let per_vertex = on
            && !self
                .render_textures
                .formats
                .taa_prep_velocity(&self.anti_aliasing);
        let was = (
            self.transforms.motion_history_enabled(),
            self.meshes.skins.motion_history_mut().0.enabled(),
        );
        if was == (on, per_vertex) {
            return;
        }
        // Rebuild (on) / drop (off) the geometry pass's TAA bind groups: off,
        // they would otherwise keep the dropped previous-frame buffers alive.
        self.bind_groups
            .mark_create(BindGroupCreate::MotionHistoryBuffers);
        self.transforms.set_motion_history(on);
        self.instances.set_motion_history(per_vertex);
        self.meshes.skins.set_motion_history(per_vertex);
        self.meshes.morphs.geometry.set_motion_history(per_vertex);
        // The fallback instead records which of them deform (non-rigid
        // marking, `update_non_rigid_gates`).
        let prep = on && !per_vertex;
        self.instances.set_track_deformed(prep);
        self.meshes.skins.set_track_deformed(prep);
        self.meshes.morphs.geometry.set_track_deformed(prep);
    }

    /// The scene resolution: swap-chain size scaled by
    /// [`Self::render_scale`] (identical to the swap-chain size at 1.0).
    /// Every scene-side consumer (geometry/shading targets, light-culling
    /// froxels, LOD screen-error, camera viewport uniform, picker, SSR, HZB)
    /// reads THIS, never the raw swap-chain size.
    pub fn render_size(&self) -> Result<(u32, u32)> {
        Ok(self.frame_sizes()?.render)
    }

    /// The post-processing resolution: what bloom, SMAA and the effects pass
    /// (haze, DoF, bloom composite, SMAA blend) run at and what the display
    /// pass reads. Equal to [`Self::render_size`] without TAA; the surface
    /// size with TAA, where the resolve writes the output resolution.
    pub fn post_size(&self) -> Result<(u32, u32)> {
        Ok(self.frame_sizes()?.post)
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
        if self.anti_aliasing.taa.is_some() && (scale - 1.0).abs() > f32::EPSILON {
            // Supersampling with TAA is not supported in v1.
            return Err(crate::error::AwsmError::TaaCombinedWithOtherAa);
        }
        if (scale - self.render_scale).abs() <= f32::EPSILON {
            return Ok(());
        }
        let was_downsampling = self.display_downsamples();
        self.render_scale = scale;
        if self.display_downsamples() != was_downsampling {
            self.render_passes
                .display
                .pipelines
                .set_render_pipeline_key(
                    &self.post_processing,
                    self.display_downsamples(),
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
