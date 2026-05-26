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
        // No-op fast path — caller asked for the state we're
        // already in. The bind-group recreate marks are skipped too;
        // there's nothing for them to invalidate.
        if self.anti_aliasing == aa {
            return Ok(());
        }
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
        let classify_first_party_entries =
            crate::dynamic_materials::first_party_bucket_entries();
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

        // Picker descriptors — only present when `features.picking`
        // is on. Returns the picker's BGLs + the (single) pipeline
        // cache key for the new MSAA. The previously-compiled
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
        let opaque_shader_jobs: Vec<crate::shaders::ShaderCacheKey> =
            opaque_descs.iter().map(|d| d.shader_cache.clone()).collect();
        let opaque_shader_keys = self
            .shaders
            .ensure_keys(&self.gpu, opaque_shader_jobs)
            .await?;

        // ── Phase 3: build compute pipeline cache keys for opaque
        //    + concatenate classify's already-resolved keys. One
        //    batched ensure_keys for the union.
        use crate::pipelines::compute_pipeline::ComputePipelineCacheKey;
        let mut compute_jobs: Vec<ComputePipelineCacheKey> = Vec::with_capacity(
            opaque_descs.len() + classify_descs.pipeline_cache_keys.len(),
        );
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
            .ensure_keys(&self.gpu, &self.shaders, &self.pipeline_layouts, compute_jobs)
            .await?;

        // ── Phase 4: merge resolved pipelines into the per-pass
        //    caches. Sync slotmap inserts; previously-compiled
        //    variants are preserved.
        let opaque_slots: Vec<_> = opaque_descs.into_iter().map(|d| d.slot).collect();
        self.render_passes
            .material_opaque
            .pipelines
            .merge_resolved(opaque_slots, resolved[opaque_pool_start..opaque_pool_end].to_vec());
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
        Ok(())
    }
}
