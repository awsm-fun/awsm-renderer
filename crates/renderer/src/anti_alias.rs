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
            smaa: false,
            mipmap: true,
        }
    }
}

impl AwsmRenderer {
    /// Updates the anti-aliasing settings and recompiles the
    /// dependent shaders + pipelines for the new config.
    ///
    /// **Lazy-pool model:** cold-boot only compiled the variants
    /// matching the initial AA config. Toggling MSAA or mipmaps
    /// here triggers a transactional recompile — two batched
    /// `ensure_keys` calls (shaders, then compute + render
    /// pipelines) cover every affected pass in parallel. Returns
    /// only after every newly-required variant is GPU-resident, so
    /// the next render frame can dispatch without further awaits.
    ///
    /// Already-compiled variants from previous MSAA states stay
    /// cached, so toggling back-and-forth pays the compile cost
    /// only on the first transition in each direction.
    pub async fn set_anti_aliasing(&mut self, aa: AntiAliasing) -> Result<()> {
        // Race policy per docs/plans/more-optimizations.md: config-change
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
        // Capture previous MSAA state before commit so the
        // edge-pipeline recompile (Block B.5) can detect off → on
        // transitions and lazily compile edge_resolve pipelines for
        // the new MSAA state. `has_msaa_checked` validates the
        // sample count here too, but a prior `set_anti_aliasing`
        // call would have rejected an unsupported count, so this is
        // effectively `is_some()` once we're past `build()`.
        let prev_msaa_on = self.anti_aliasing.has_msaa_checked()?;
        self.anti_aliasing = aa;
        self.bind_groups
            .mark_create(BindGroupCreate::AntiAliasingChange);
        self.bind_groups
            .mark_create(BindGroupCreate::TextureViewRecreate);

        // ── Phase 1: collect descriptors for every pass affected by
        //    an AA change. Each builder returns "the cache keys this
        //    pass needs for the *new* config" — descriptors for
        //    already-compiled variants resolve as cache hits inside
        //    `ensure_keys`, so the cross-pass batches stay sized to
        //    the *new* work only.
        let opaque_descs =
            crate::render_passes::material_opaque::pipeline::MaterialOpaquePipelines::shader_descriptors_for_config(
                &self.gpu,
                &mut self.bind_group_layouts,
                &mut self.pipeline_layouts,
                &self.render_passes.material_opaque.bind_groups,
                &self.anti_aliasing,
            )?;
        let classify_first_party_entries = crate::dynamic_materials::first_party_bucket_entries();
        let classify_descs =
            crate::render_passes::material_classify::pipeline::MaterialClassifyPipelines::build_descriptors_for_config(
                &self.gpu,
                &mut self.bind_group_layouts,
                &mut self.pipeline_layouts,
                &mut self.shaders,
                &self.render_passes.material_classify.bind_groups,
                &classify_first_party_entries,
                &self.anti_aliasing,
            )
            .await?;

        // HZB descriptors — only present when `features.gpu_culling`
        // is on. The unwrap-or-default path keeps the rest of the
        // pool size invariant when HZB isn't allocated.
        let hzb_descs = if let Some(hzb) = self.render_passes.hzb.as_ref() {
            Some(
                crate::render_passes::hzb::pipeline::HzbPipelines::build_descriptors_for_config(
                    &self.gpu,
                    &mut self.bind_group_layouts,
                    &mut self.pipeline_layouts,
                    &mut self.shaders,
                    &hzb.bind_groups,
                    &self.anti_aliasing,
                )
                .await?,
            )
        } else {
            None
        };

        // Picker descriptors — only present when the picker has been
        // lazily compiled (Block B.4: `self.picker` stays `None` until
        // first `pick()` query even when `features.picking == true`).
        // When the picker isn't yet built, this whole block is skipped —
        // the next `pick()` will compile it for the live AA config.
        // When it IS built, returns the picker's BGLs + the (single)
        // pipeline cache key for the new MSAA. The previously-compiled
        // variant on `self.picker` is preserved via `merge_resolved`.
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

        // ── Phase 2: batch shader compile for opaque (classify
        //    already resolved its shader inside `build_descriptors_for_config`'s
        //    `get_key` call). One ensure_keys for everything else.
        let opaque_shader_jobs: Vec<crate::shaders::ShaderCacheKey> = opaque_descs
            .iter()
            .map(|d| d.shader_cache.clone())
            .collect();
        let opaque_shader_keys = self
            .shaders
            .ensure_keys(&self.gpu, opaque_shader_jobs)
            .await?;

        // ── Phase 3: build compute pipeline cache keys for opaque
        //    + concatenate classify's already-resolved keys. One
        //    batched ensure_keys for the union.
        use crate::pipelines::compute_pipeline::ComputePipelineCacheKey;
        let mut compute_jobs: Vec<ComputePipelineCacheKey> =
            Vec::with_capacity(opaque_descs.len() + classify_descs.pipeline_cache_keys.len());
        let opaque_pool_start = 0;
        for (desc, shader_key) in opaque_descs.iter().zip(&opaque_shader_keys) {
            compute_jobs.push(ComputePipelineCacheKey::new(*shader_key, desc.layout_key));
        }
        let opaque_pool_end = compute_jobs.len();
        let classify_pool_start = compute_jobs.len();
        compute_jobs.extend(classify_descs.pipeline_cache_keys.iter().cloned());
        let classify_pool_end = compute_jobs.len();

        // HZB pool slice (when present).
        let hzb_range = hzb_descs.as_ref().map(|d| {
            let start = compute_jobs.len();
            compute_jobs.extend(d.pipeline_cache_keys.iter().cloned());
            start..compute_jobs.len()
        });

        // Picker pool slice (when present).
        let picker_range = picker_descs.as_ref().map(|d| {
            let start = compute_jobs.len();
            compute_jobs.extend(d.pipeline_cache_keys.iter().cloned());
            start..compute_jobs.len()
        });

        let resolved = self
            .pipelines
            .compute
            .ensure_keys(
                &self.gpu,
                &self.shaders,
                &self.pipeline_layouts,
                compute_jobs,
            )
            .await?;

        // ── Phase 4: merge resolved pipelines into the per-pass
        //    caches. Sync slotmap inserts; previously-compiled
        //    variants are preserved.
        let opaque_slots: Vec<_> = opaque_descs.into_iter().map(|d| d.slot).collect();
        self.render_passes.material_opaque.pipelines.merge_resolved(
            opaque_slots,
            resolved[opaque_pool_start..opaque_pool_end].to_vec(),
        );
        self.render_passes
            .material_classify
            .pipelines
            .merge_resolved(
                classify_descs.slot_msaa,
                resolved[classify_pool_start..classify_pool_end].to_vec(),
            );
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

        // ── Phase 5: transparent pipelines depend on per-mesh
        //    attributes AND AA settings — recompile every live
        //    mesh's variant. Batched inside `set_render_pipeline_keys_batched`.
        let mut requests: Vec<
            crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest,
        > = Vec::new();
        for (mesh_key, mesh) in self.meshes.iter() {
            let buffer_info_key = self.meshes.buffer_info_key(mesh_key)?;
            let has_transmission = self.materials.has_transmission(mesh.material_key);
            requests.push(
                crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest {
                    mesh,
                    mesh_key,
                    buffer_info_key,
                    has_transmission,
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
            )
            .await?;

        // ── Phase 7 (Block B.5): edge_resolve pipeline lazy compile
        //    on MSAA off → on. Cold-boot only compiled edge_resolve
        //    pipelines when MSAA was on at startup; without this,
        //    toggling MSAA off → on after build leaves them empty
        //    and the render-frame preamble's `warn_pipeline_not_compiled`
        //    silently skips them, so cross-material MSAA edges fall
        //    through.
        //
        //    Only fires on the off → on edge AND when the device
        //    actually supports the required limits — mirrors the
        //    `edge_resolve_enabled` gate at the build site
        //    (`lib.rs` ~1390). on → off and on → on (4 → 4 — already
        //    short-circuited above) and off → off paths skip; their
        //    dispatch sites are already guarded.
        let new_msaa_on = multisampled_geometry;
        if !prev_msaa_on
            && new_msaa_on
            && crate::edge_resolve_supported(&self.gpu)
        {
            let color_wgsl = awsm_renderer_core::texture::texture_format_to_wgsl_storage(
                self.render_textures.formats.color,
            )?;
            let bucket_entries = self
                .dynamic_materials
                .bucket_entries_cached()
                .to_vec();
            let crate::pipelines::Pipelines {
                render: _render_pipelines,
                compute: compute_pipelines,
            } = &mut self.pipelines;
            self.render_passes
                .material_opaque
                .edge_pipelines
                .ensure_compiled(
                    &self.gpu,
                    &mut self.shaders,
                    compute_pipelines,
                    &mut self.pipeline_layouts,
                    &mut self.bind_group_layouts,
                    &self.render_passes.material_opaque.bind_groups,
                    &self.render_passes.material_opaque.edge_bind_group_layouts,
                    &bucket_entries,
                    &self.anti_aliasing,
                    color_wgsl,
                    Some(&self.dynamic_materials),
                )
                .await?;
        }

        // ── Phase 8 (Block B.3): line pipelines lazy ensure. Cold-boot
        //    leaves the 4 line-pipeline variants uncompiled; the first
        //    `add_line_*` flips `pipelines_compile_requested`. The 4
        //    variants already cover the MSAA cross product (compile
        //    once, valid for both states), so this is effectively a
        //    no-op on MSAA flip once the variants are populated — but
        //    if a line was registered after a previous
        //    `wait_for_pipelines_ready` AND `set_anti_aliasing` is the
        //    next API call before another `wait_for_pipelines_ready`,
        //    we still want the dispatch to render the line under the
        //    new AA state instead of warn-skipping.
        if !self.lines.is_empty() || self.lines.pipelines_compile_requested() {
            self.ensure_line_pipelines_compiled().await?;
        }
        Ok(())
    }
}
