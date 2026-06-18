//! Texture management and GPU uploads.

use std::{collections::HashMap, sync::LazyLock};

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    compare::CompareFunction,
    cubemap::{self, CubemapBytesLayout, CubemapFace},
    error::AwsmCoreError,
    image::ImageData,
    renderer::AwsmRendererWebGpu,
    sampler::{AddressMode, FilterMode, MipmapFilterMode, SamplerDescriptor},
    texture::{
        texture_pool::{TextureColorInfo, TexturePool, TexturePoolEntryInfo},
        TextureFormat,
    },
};
use indexmap::IndexSet;
use ordered_float::OrderedFloat;
use slotmap::{new_key_type, SecondaryMap, SlotMap};
use thiserror::Error;

use crate::{
    bind_groups::{BindGroupCreate, BindGroups},
    buffer::dynamic_uniform::DynamicUniformBuffer,
    error::AwsmError,
    render_passes::RenderPassInitContext,
    AwsmRenderer, AwsmRendererLogging,
};

static TEXTURE_TRANSFORM_BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_storage().with_copy_dst());

/// Initial capacity for texture transform storage.
pub const TEXTURE_TRANSFORMS_INITIAL_CAPACITY: usize = 32; // 32 elements is a good starting point
/// Byte size for a single texture transform.
pub const TEXTURE_TRANSFORMS_BYTE_SIZE: usize = 32; // 32 bytes per texture transform (must match shader struct size)

impl AwsmRenderer {
    // this should ideally only be called after all the textures have been loaded
    /// Uploads texture pool data and refreshes dependent pipelines.
    ///
    /// On a cold PSO disk cache this is the biggest single phase of
    /// boot-time wall-clock by a wide margin: every render-pass that
    /// indexes into the texture pool (opaque + decal + transparent)
    /// has its shader text templated against the pool's
    /// `(arrays_len, samplers_len)`, so when the pool grows from 0
    /// to the real count, every existing PSO is invalidated and the
    /// driver has to compile every variant from scratch.
    ///
    /// Architecture: bind-group rebuilds run synchronously up front
    /// (no Dawn work), then a single `Shaders::ensure_keys` pools the
    /// opaque + decal + per-mesh-transparent shader cache keys, then
    /// a single `ComputePipelines::ensure_keys` pools opaque + decal
    /// pipeline cache keys, then a single
    /// `RenderPipelines::ensure_keys` pools the per-mesh transparent
    /// keys. Three awaits total instead of the previous six (per
    /// pass: one shader-batch + one pipeline-batch × three passes),
    /// each running its compiles in parallel through Dawn's pool.
    /// Read back a pooled texture as PNG bytes (GPU→CPU). Looks up the texture's
    /// array + layer + dimensions, copies that layer to a mappable buffer, and
    /// encodes a PNG (sRGB-corrected per the texture's upload colour space).
    /// Used by the editor's image-query seam to snapshot file/raster textures.
    #[cfg(feature = "texture-export")]
    pub async fn texture_png_bytes(
        &self,
        key: TextureKey,
    ) -> std::result::Result<Vec<u8>, AwsmError> {
        let (array_index, layer_index, srgb) = {
            let e = self.textures.get_entry(key)?;
            (e.array_index, e.layer_index, e.color.srgb_to_linear)
        };
        let (texture, width, height, format) = {
            let array = self
                .textures
                .pool
                .array_by_index(array_index)
                .ok_or(AwsmTextureError::TextureNotFound(key))?;
            let texture = array
                .gpu_texture
                .clone()
                .ok_or(AwsmTextureError::TextureNotFound(key))?;
            (texture, array.width, array.height, array.format)
        };
        let png = self
            .gpu
            .export_texture_as_png(
                &texture,
                width,
                height,
                layer_index as u32,
                format,
                None,
                false,
                Some(srgb),
            )
            .await?;
        Ok(png)
    }

    pub async fn finalize_gpu_textures(&mut self) -> std::result::Result<(), AwsmError> {
        // Take the sampler-pool dirty bit *before* the pool write —
        // both bits feed the same rebuild gate below. Without this OR,
        // `ensure_sampler_in_pool` (cache-hit texture bound to a
        // not-yet-pooled sampler) would land in the set but the
        // texture-pool bind group + dependent pipeline layouts would
        // still reflect the old sampler count, so `sampler_index()`
        // would point past the end of the bound sampler array. The
        // editor's `MaterialDef` override + the particles_sync path
        // are the canonical callers.
        let sampler_pool_dirty = self.textures.take_sampler_pool_dirty();
        let pool_dirty = self
            .textures
            .write_gpu_texture_pool(&self.logging, &self.gpu)
            .await?;
        // A custom-material register/alpha-edit can need the masked variant
        // (re)built with no texture change (procedural cutout). Take the flag so
        // the rebuild below runs; with an unchanged pool the opaque/transparent
        // descriptors below are cache hits and only the new masked variant compiles.
        let force_masked = std::mem::take(&mut self.masked_dynamic_dirty);
        let was_dirty = pool_dirty || sampler_pool_dirty;

        if !was_dirty && !force_masked {
            return Ok(());
        }

        self.bind_groups.mark_create(BindGroupCreate::TexturePool);

        // -----------------------------------------------------------
        // Phase A — sync bind-group + pipeline-layout rebuild
        // -----------------------------------------------------------
        // Every pass that indexes into the texture pool needs its
        // bind-group + pipeline-layout cache entry rebuilt against
        // the new pool dimensions. These are pure hash registrations
        // — no Dawn compile work.
        let opaque_bind_groups = {
            let mut render_pass_ctx = RenderPassInitContext {
                gpu: &mut self.gpu,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                textures: &mut self.textures,
                render_texture_formats: &mut self.render_textures.formats,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            self.render_passes
                .material_opaque
                .bind_groups
                .clone_because_texture_pool_changed(&mut render_pass_ctx)?
        };
        let transparent_bind_groups = {
            let mut render_pass_ctx = RenderPassInitContext {
                gpu: &mut self.gpu,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                textures: &mut self.textures,
                render_texture_formats: &mut self.render_textures.formats,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            self.render_passes
                .material_transparent
                .bind_groups
                .clone_because_texture_pool_changed(&mut render_pass_ctx)?
        };
        let decal_bind_groups = if let Some(decal) = self.render_passes.material_decal.as_ref() {
            let mut render_pass_ctx = RenderPassInitContext {
                gpu: &mut self.gpu,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                textures: &mut self.textures,
                render_texture_formats: &mut self.render_textures.formats,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            Some(
                decal
                    .bind_groups
                    .clone_because_texture_pool_changed(&mut render_pass_ctx)?,
            )
        } else {
            None
        };

        self.render_passes.material_opaque.bind_groups = opaque_bind_groups;
        self.render_passes.material_transparent.bind_groups = transparent_bind_groups;
        if let Some(new_bg) = decal_bind_groups {
            if let Some(decal) = self.render_passes.material_decal.as_mut() {
                decal.bind_groups = new_bg;
            }
        }
        // Transparent has no global pipeline set, just a layout key
        // that gets used by the per-mesh batch below — refresh it.
        self.render_passes
            .material_transparent
            .pipelines
            .refresh_pipeline_layout(
                &self.gpu,
                &mut self.bind_group_layouts,
                &mut self.pipeline_layouts,
                &self.render_passes.material_transparent.bind_groups,
            )?;

        // -----------------------------------------------------------
        // Phase B — collect every shader cache key across every pass
        // -----------------------------------------------------------
        use crate::render_passes::material_transparent::pipeline::TransparentMeshPipelineRequest;

        // Build one request per mesh. The previous OR-style dedup
        // ("skip if buffer_info OR material was already seen") was a
        // pre-existing bug — for mesh sets like (A,M1), (B,M2),
        // (A,M2), (B,M1) it skips the third / fourth pair even
        // though they produce different pipeline cache keys (e.g.
        // when M1 and M2 differ in `writes_depth`), leaving
        // those meshes with stale pipeline-key map entries after
        // the layout change. `Shaders::ensure_keys` and
        // `RenderPipelines::ensure_keys` both dedupe internally by
        // their cache keys, so the cost of sending all meshes is
        // just a couple of extra hash probes per mesh.
        let mut transparent_requests: Vec<TransparentMeshPipelineRequest> = Vec::new();
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
            transparent_requests.push(TransparentMeshPipelineRequest {
                mesh,
                mesh_key,
                buffer_info_key,
                writes_depth,
                base,
                pbr_features,
                dynamic_shader_id,
                dynamic_shader,
            });
        }

        let mut all_shader_keys: Vec<crate::shaders::ShaderCacheKey> = Vec::new();
        {
            let mut render_pass_ctx = RenderPassInitContext {
                gpu: &mut self.gpu,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                textures: &mut self.textures,
                render_texture_formats: &mut self.render_textures.formats,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            all_shader_keys.extend(
                crate::render_passes::material_opaque::pipeline::MaterialOpaquePipelines::build_shader_cache_keys(
                    &mut render_pass_ctx,
                    &self.render_passes.material_opaque.bind_groups,
                )?,
            );
            if let Some(decal) = self.render_passes.material_decal.as_ref() {
                all_shader_keys.extend(
                    crate::render_passes::material_decal::pipeline::MaterialDecalPipelines::build_shader_cache_keys(
                        &mut render_pass_ctx,
                        &decal.bind_groups,
                    )?,
                );
            }
        }
        all_shader_keys.extend(
            crate::render_passes::material_transparent::pipeline::MaterialTransparentPipelines::shader_cache_keys_for_requests(
                transparent_requests.iter(),
                &self.render_passes.material_transparent.bind_groups,
                &self.meshes.buffer_infos,
                &self.anti_aliasing,
            )?
            .into_iter()
            .map(crate::shaders::ShaderCacheKey::from),
        );

        // Single cross-pass shader compile batch.
        self.shaders.ensure_keys(&self.gpu, all_shader_keys).await?;

        // -----------------------------------------------------------
        // Phase C — build pipeline cache keys (shaders are warm)
        // -----------------------------------------------------------
        let (opaque_descs, decal_descs) = {
            let mut render_pass_ctx = RenderPassInitContext {
                gpu: &mut self.gpu,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                textures: &mut self.textures,
                render_texture_formats: &mut self.render_textures.formats,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            let opaque_descs = crate::render_passes::material_opaque::pipeline::MaterialOpaquePipelines::build_descriptors(
                &mut render_pass_ctx,
                &self.render_passes.material_opaque.bind_groups,
            )
            .await?;
            let decal_descs = if let Some(decal) = self.render_passes.material_decal.as_ref() {
                Some(
                    crate::render_passes::material_decal::pipeline::MaterialDecalPipelines::build_descriptors(
                        &mut render_pass_ctx,
                        &decal.bind_groups,
                    )
                    .await?,
                )
            } else {
                None
            };
            (opaque_descs, decal_descs)
        };

        let transparent_pipeline_cache_keys = self
            .render_passes
            .material_transparent
            .pipelines
            .pipeline_cache_keys_for_requests(
                &self.gpu,
                transparent_requests.iter(),
                &mut self.shaders,
                &self.render_passes.material_transparent.bind_groups,
                &self.meshes.buffer_infos,
                &self.anti_aliasing,
                &self.render_textures.formats,
            )
            .await?;

        // -----------------------------------------------------------
        // Phase D — one batched compute + one batched render compile
        // -----------------------------------------------------------
        let opaque_pipeline_count = opaque_descs.pipeline_cache_keys.len();
        let mut compute_cache_keys = opaque_descs.pipeline_cache_keys.clone();
        if let Some(ref decal_descs) = decal_descs {
            compute_cache_keys.extend(decal_descs.pipeline_cache_keys.iter().cloned());
        }

        let compute_pipeline_keys = self
            .pipelines
            .compute
            .ensure_keys(
                &self.gpu,
                &self.shaders,
                &self.pipeline_layouts,
                compute_cache_keys,
            )
            .await?;

        let transparent_pipeline_keys = self
            .pipelines
            .render
            .ensure_keys(
                &self.gpu,
                &self.shaders,
                &self.pipeline_layouts,
                transparent_pipeline_cache_keys,
            )
            .await?;

        // -----------------------------------------------------------
        // Phase E — sync fold-up of resolved keys
        // -----------------------------------------------------------
        let (opaque_keys_slice, decal_keys_slice) =
            compute_pipeline_keys.split_at(opaque_pipeline_count);
        self.render_passes.material_opaque.pipelines =
            crate::render_passes::material_opaque::pipeline::MaterialOpaquePipelines::from_resolved(
                opaque_descs.slots,
                opaque_keys_slice.to_vec(),
            );
        if let (Some(decal_descs), Some(decal)) =
            (decal_descs, self.render_passes.material_decal.as_mut())
        {
            decal.pipelines =
                crate::render_passes::material_decal::pipeline::MaterialDecalPipelines::from_resolved(
                    decal_descs.is_msaa,
                    decal_keys_slice.to_vec(),
                );
        }
        let mesh_keys: Vec<_> = transparent_requests.iter().map(|r| r.mesh_key).collect();
        self.render_passes
            .material_transparent
            .pipelines
            .install_per_mesh_keys(mesh_keys, transparent_pipeline_keys);

        // -----------------------------------------------------------
        // Masked (alpha-tested) geometry — rebuild against the new pool
        // -----------------------------------------------------------
        // The masked group-0 carries the texture pool, so its layout changes
        // when the pool grows. Relayout the bind group + pipeline pool, then
        // (re)compile the built-in PBR masked variant (its base-color cutout
        // samples the pool), plus every registered MASK custom material that
        // carries a 2nd alpha-only WGSL window.
        {
            // Collect the registered MASK customs first (releases the
            // dynamic_materials borrow before the RenderPassInitContext below).
            let custom_masked: Vec<(
                awsm_materials::MaterialShaderId,
                crate::render_passes::geometry::shader::masked_cache_key::DynamicAlphaShaderInfo,
            )> = self
                .dynamic_materials
                .iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
                .into_iter()
                .filter_map(|id| {
                    self.dynamic_materials
                        .alpha_info_for(id)
                        .map(|info| (id, info))
                })
                .collect();

            let new_masked_bg = {
                let mut ctx = RenderPassInitContext {
                    gpu: &mut self.gpu,
                    pipelines: &mut self.pipelines,
                    shaders: &mut self.shaders,
                    textures: &mut self.textures,
                    render_texture_formats: &mut self.render_textures.formats,
                    bind_group_layouts: &mut self.bind_group_layouts,
                    pipeline_layouts: &mut self.pipeline_layouts,
                    features: &self.features,
                    anti_aliasing: &self.anti_aliasing,
                    post_processing: &self.post_processing,
                    prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
                };
                self.render_passes
                    .geometry
                    .masked_bind_group
                    .clone_because_texture_pool_changed(&mut ctx)?
            };
            self.render_passes.geometry.masked_bind_group = new_masked_bg;

            let mut ctx = RenderPassInitContext {
                gpu: &mut self.gpu,
                pipelines: &mut self.pipelines,
                shaders: &mut self.shaders,
                textures: &mut self.textures,
                render_texture_formats: &mut self.render_textures.formats,
                bind_group_layouts: &mut self.bind_group_layouts,
                pipeline_layouts: &mut self.pipeline_layouts,
                features: &self.features,
                anti_aliasing: &self.anti_aliasing,
                post_processing: &self.post_processing,
                prep_config: &self.prep_config,
                max_edge_budget: self.material_edge_buffers.as_ref().map(|b| b.max_edge_budget).unwrap_or(crate::render_passes::material_opaque::edge_buffers::DEFAULT_MAX_EDGE_BUDGET_DESKTOP),
            };
            self.render_passes.geometry.masked_pipelines.relayout(
                &mut ctx,
                &self.render_passes.geometry.masked_bind_group,
                &self.render_passes.geometry.bind_groups,
            )?;
            // Built-in MASK materials route alpha-tested-OPAQUE: PBR, Unlit,
            // Toon share the same header prefix (shader_id, alpha_mode,
            // alpha_cutoff, base_color_tex(5), base_color_factor(4)), so the
            // masked fragment's base-color path covers them with one WGSL —
            // only the cache-key shader_id differs. FlipBook gets its OWN
            // masked WGSL arm (the mask alpha is the time-varying atlas cell,
            // evaluated by the shared cell math the shaded material also runs).
            for (shader_id, base) in [
                (
                    awsm_materials::MaterialShaderId::PBR,
                    crate::dynamic_materials::ShadingBase::Pbr,
                ),
                (
                    awsm_materials::MaterialShaderId::UNLIT,
                    crate::dynamic_materials::ShadingBase::Unlit,
                ),
                (
                    awsm_materials::MaterialShaderId::TOON,
                    crate::dynamic_materials::ShadingBase::Toon,
                ),
                (
                    awsm_materials::MaterialShaderId::FLIPBOOK,
                    crate::dynamic_materials::ShadingBase::Flipbook,
                ),
            ] {
                let variant = crate::render_passes::geometry::masked_pipeline::MaskedVariant {
                    shader_id,
                    base,
                    dynamic_alpha: None,
                };
                self.render_passes
                    .geometry
                    .masked_pipelines
                    .ensure_variant(
                        &mut ctx,
                        &self.render_passes.geometry.masked_bind_group,
                        &variant,
                    )
                    .await?;
            }

            // Custom MASK materials — one masked variant each, emitting the
            // author's alpha-only fragment. Iterate by reference so the same
            // list feeds the masked-shadow build below.
            for (shader_id, info) in &custom_masked {
                let variant = crate::render_passes::geometry::masked_pipeline::MaskedVariant {
                    shader_id: *shader_id,
                    base: crate::dynamic_materials::ShadingBase::Custom,
                    dynamic_alpha: Some(info.clone()),
                };
                self.render_passes
                    .geometry
                    .masked_pipelines
                    .ensure_variant(
                        &mut ctx,
                        &self.render_passes.geometry.masked_bind_group,
                        &variant,
                    )
                    .await?;
            }

            // -------------------------------------------------------------
            // Masked (alpha-tested) SHADOW casters — same per-shader-id pool,
            // for hole-shaped (cutout) shadows (B2). The masked-shadow group-0
            // carries the texture pool too, so relayout it against the new pool,
            // then compile the built-in PBR/Unlit/Toon variants (base-color
            // cutout) + every registered MASK custom (alpha-only WGSL). The
            // shadow render path falls back to the solid pipeline until these
            // land, so a masked caster always casts *some* shadow.
            // -------------------------------------------------------------
            let new_shadow_masked_bg = self
                .render_passes
                .shadow_masked
                .bind_group
                .clone_because_texture_pool_changed(&mut ctx)?;
            self.render_passes.shadow_masked.bind_group = new_shadow_masked_bg;
            self.render_passes.shadow_masked.pipelines.relayout(
                &mut ctx,
                &self.render_passes.shadow_masked.bind_group,
                &self.render_passes.geometry.bind_groups,
            )?;
            for (shader_id, base) in [
                (
                    awsm_materials::MaterialShaderId::PBR,
                    crate::dynamic_materials::ShadingBase::Pbr,
                ),
                (
                    awsm_materials::MaterialShaderId::UNLIT,
                    crate::dynamic_materials::ShadingBase::Unlit,
                ),
                (
                    awsm_materials::MaterialShaderId::TOON,
                    crate::dynamic_materials::ShadingBase::Toon,
                ),
                (
                    awsm_materials::MaterialShaderId::FLIPBOOK,
                    crate::dynamic_materials::ShadingBase::Flipbook,
                ),
            ] {
                let variant = crate::render_passes::shadow_masked::pipeline::MaskedShadowVariant {
                    shader_id,
                    base,
                    dynamic_alpha: None,
                };
                self.render_passes
                    .shadow_masked
                    .pipelines
                    .ensure_variant(
                        &mut ctx,
                        &self.render_passes.shadow_masked.bind_group,
                        &variant,
                    )
                    .await?;
            }
            for (shader_id, info) in &custom_masked {
                let variant = crate::render_passes::shadow_masked::pipeline::MaskedShadowVariant {
                    shader_id: *shader_id,
                    base: crate::dynamic_materials::ShadingBase::Custom,
                    dynamic_alpha: Some(info.clone()),
                };
                self.render_passes
                    .shadow_masked
                    .pipelines
                    .ensure_variant(
                        &mut ctx,
                        &self.render_passes.shadow_masked.bind_group,
                        &variant,
                    )
                    .await?;
            }
        }

        // Re-launch the compile for every currently-registered
        // scheduler material entry.
        //
        // The opaque-pipeline rebuild above only emits descriptors for
        // the `OpaquePipelineSlot::Empty*` slots (`MaterialOpaquePipelines::
        // shader_descriptors_and_layouts` passes `include_first_party:
        // false`), and `from_resolved` constructs the typed cache
        // from those slots wholesale — wiping any first-party /
        // dynamic material pipeline keys that were previously
        // compiled. That wipe is intentional: their underlying
        // shaders were compiled with the OLD `texture_pool_arrays_len`
        // template substitution, and the new `texture_pool_textures`
        // BGL doesn't match the OLD pipeline's layout, so reusing
        // those keys would either silently sample the
        // `default → vec4(0)` branch (the bug that motivated the
        // Stage 3 silhouette-quality fix in `53202fa`) or fail
        // outright at dispatch validation.
        //
        // Mirror that wipe on the edge_pipelines per-pass cache: the
        // edge_resolve shaders also depend on the OLD
        // `texture_pool_arrays_len`, and after the wipe above the
        // dispatch path skips the affected materials via the Option
        // guards on `get_compute_pipeline_key`. Without clearing
        // `edge_pipelines.per_shader` + the global skybox /
        // final_blend keys here too, edge dispatch would still hit
        // the OLD-pool-shape pipelines until the new ones land.
        self.render_passes
            .material_opaque
            .edge_pipelines
            .clear_dynamic_pipelines();

        // Render-driven recompile against the new texture-pool layout. A
        // pool grow doesn't change the bucket SET (dispatch_hash / count
        // unchanged), so it wouldn't trip `ensure_scene_pipelines`'
        // layout-change detector on its own — but it DOES invalidate every
        // opaque/classify/edge pipeline (their cache keys embed
        // `texture_pool_arrays_len` / `texture_pool_samplers_len`). Reset
        // the ensure fingerprint to force the next preamble to treat this
        // as a layout change: it re-runs the buffer-relayout (idempotent —
        // the buffers are already the right size, so a no-op) + the
        // cache-clear + generation-bump (dropping any in-flight old-pool
        // resolutions), then recompiles every bucket against the new pool.
        // The `mark_variants_dirty` flag is what actually drives that
        // ensure on the next frame.
        self.last_ensured_bucket_layout = None;
        self.materials.mark_variants_dirty();

        // Rebuild the full edge-resolve set against the new texture-pool
        // layout (its cache keys embed `texture_pool_arrays_len` /
        // `texture_pool_samplers_len`). This handler is async, so it uses
        // the authoritative awaited `ensure_compiled` — the same path the
        // MSAA-change + cold-boot/load paths use — for ALL cases (with or
        // without registered materials), not just the no-materials one.
        if self.anti_aliasing.msaa_sample_count.is_some()
            && crate::edge_resolve_supported(&self.gpu)
        {
            let color_wgsl = awsm_renderer_core::texture::texture_format_to_wgsl_storage(
                self.render_textures.formats.color,
            )?;
            let bucket_entries = self.dynamic_materials.bucket_entries_cached().to_vec();
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
                    self.prep_config.clamped_k(),
                )
                .await?;
        }

        Ok(())
    }

    /// Updates one face of a cubemap texture in-place from raw bytes.
    pub fn update_cubemap_texture_face(
        &self,
        texture_key: CubemapTextureKey,
        face: CubemapFace,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
        layout: CubemapBytesLayout,
    ) -> crate::error::Result<()> {
        let texture = self.textures.get_cubemap(texture_key)?;
        cubemap::update_texture_face(
            &self.gpu, texture, face, mip_level, width, height, data, layout,
        )?;
        Ok(())
    }

    /// Updates all six faces of a cubemap texture in-place from one contiguous byte buffer.
    ///
    /// Data must be packed in face order: +X, -X, +Y, -Y, +Z, -Z.
    pub fn update_cubemap_texture_all_faces(
        &self,
        texture_key: CubemapTextureKey,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
        layout: CubemapBytesLayout,
    ) -> crate::error::Result<()> {
        let texture = self.textures.get_cubemap(texture_key)?;
        cubemap::update_texture_all_faces(
            &self.gpu, texture, mip_level, width, height, data, layout,
        )?;
        Ok(())
    }

    /// Removes a pool texture. Consumers should ensure no live material
    /// still binds `key` (i.e. trigger a re-resolve cascade before
    /// calling), then drop their cached handle. Returns `true` if the
    /// key existed; `false` if it was already gone.
    pub fn remove_texture(&mut self, key: TextureKey) -> bool {
        self.textures.remove(key)
    }

    /// Regenerates mipmaps for an existing cubemap texture.
    pub async fn regenerate_cubemap_texture_mipmaps(
        &self,
        texture_key: CubemapTextureKey,
        mip_levels: u32,
    ) -> crate::error::Result<()> {
        let texture = self.textures.get_cubemap(texture_key)?;
        cubemap::regenerate_texture_mipmaps(&self.gpu, texture, mip_levels).await?;
        Ok(())
    }
}

/// Texture pool, samplers, and texture transforms.
pub struct Textures {
    pub pool: TexturePool<TextureKey>,
    pub pool_sampler_set: IndexSet<SamplerKey>,
    pub texture_transform_identity_offset: usize,
    pool_textures: SlotMap<TextureKey, TexturePoolEntryInfo<TextureKey>>,
    cubemaps: SlotMap<CubemapTextureKey, web_sys::GpuTexture>,
    samplers: SlotMap<SamplerKey, web_sys::GpuSampler>,
    sampler_cache: HashMap<SamplerCacheKey, SamplerKey>,
    // We keep a mirror of the sampler address modes so that materials can adjust UVs manually when
    sampler_address_modes: SecondaryMap<SamplerKey, (Option<AddressMode>, Option<AddressMode>)>,
    texture_transforms: SlotMap<TextureTransformKey, ()>,
    texture_transforms_buffer: DynamicUniformBuffer<TextureTransformKey>,
    texture_transforms_gpu_dirty: bool,
    pub(crate) texture_transforms_gpu_buffer: web_sys::GpuBuffer,
    texture_transforms_uploader: crate::buffer::mapped_uploader::MappedUploader,
    /// Set when `pool_sampler_set` mutates without an accompanying
    /// pool-array `gpu_dirty` flip (i.e. `ensure_sampler_in_pool` or
    /// `add_image` inserting a sampler that wasn't already present).
    /// `finalize_gpu_textures` ORs this with the pool-write dirty bit
    /// when deciding whether to rebuild material bind groups /
    /// pipeline layouts — without it, a new sampler would land in the
    /// set but `sampler_index()` would point past the end of the
    /// previously-cached bind group's sampler array.
    sampler_pool_dirty: bool,
}

/// Cache key for samplers.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct SamplerCacheKey {
    pub address_mode_u: Option<AddressMode>,
    pub address_mode_v: Option<AddressMode>,
    pub address_mode_w: Option<AddressMode>,
    pub compare: Option<CompareFunction>,
    pub lod_min_clamp: Option<OrderedFloat<f32>>,
    pub lod_max_clamp: Option<OrderedFloat<f32>>,
    pub max_anisotropy: Option<u16>,
    pub mag_filter: Option<FilterMode>,
    pub min_filter: Option<FilterMode>,
    pub mipmap_filter: Option<MipmapFilterMode>,
}

impl SamplerCacheKey {
    /// Returns true if anisotropy is allowed with the current filters.
    pub fn allowed_ansiotropy(&self) -> bool {
        match (self.min_filter, self.mag_filter, self.mipmap_filter) {
            (Some(FilterMode::Nearest), _, _)
            | (_, Some(FilterMode::Nearest), _)
            | (_, _, Some(MipmapFilterMode::Nearest)) => false,
            _ => true,
        }
    }
}

impl std::hash::Hash for SamplerCacheKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.address_mode_u.map(|x| x as u32).hash(state);
        self.address_mode_v.map(|x| x as u32).hash(state);
        self.address_mode_w.map(|x| x as u32).hash(state);
        self.compare.map(|x| x as u32).hash(state);
        self.lod_min_clamp.hash(state);
        self.lod_max_clamp.hash(state);
        self.max_anisotropy.hash(state);
        self.mag_filter.map(|x| x as u32).hash(state);
        self.min_filter.map(|x| x as u32).hash(state);
        self.mipmap_filter.map(|x| x as u32).hash(state);
    }
}

/// Texture transform parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureTransform {
    pub offset: [f32; 2],
    pub origin: [f32; 2],
    pub rotation: f32,
    pub scale: [f32; 2],
}

impl TextureTransform {
    /// Returns an identity transform.
    pub fn identity() -> Self {
        Self {
            offset: [0.0, 0.0],
            origin: [0.0, 0.0],
            rotation: 0.0,
            scale: [1.0, 1.0],
        }
    }

    /// Packs the transform into GPU bytes.
    pub fn as_gpu_bytes(&self) -> [u8; TEXTURE_TRANSFORMS_BYTE_SIZE] {
        let mut bytes = [0u8; TEXTURE_TRANSFORMS_BYTE_SIZE];

        let sx = self.scale[0];
        let sy = self.scale[1];
        let ox = self.offset[0];
        let oy = self.offset[1];
        let px = self.origin[0];
        let py = self.origin[1];

        let c = self.rotation.cos();
        let s = self.rotation.sin();

        // M = R * S
        // glTF rotation matrix (counter-clockwise, with V pointing down):
        // [ cos   sin ] * [ sx  0  ]   =   [ cos*sx   sin*sy ]
        // [ -sin  cos ]   [ 0   sy ]       [ -sin*sx  cos*sy ]
        let m00 = c * sx;
        let m01 = s * sy;
        let m10 = -s * sx;
        let m11 = c * sy;

        // B = offset + origin - M * origin
        let mx_px = m00 * px + m01 * py;
        let my_py = m10 * px + m11 * py;

        let bx = ox + px - mx_px;
        let by = oy + py - my_py;

        bytes[0..4].copy_from_slice(&m00.to_le_bytes());
        bytes[4..8].copy_from_slice(&m01.to_le_bytes());
        bytes[8..12].copy_from_slice(&m10.to_le_bytes());
        bytes[12..16].copy_from_slice(&m11.to_le_bytes());
        bytes[16..20].copy_from_slice(&bx.to_le_bytes());
        bytes[20..24].copy_from_slice(&by.to_le_bytes());

        bytes
    }
}

impl Textures {
    /// Live GPU-texture-resource counts `(pool_textures, cubemaps, samplers)` for
    /// leak diagnostics (surfaced via `memory_stats`). `memory_stats` historically
    /// counted only pipelines/shaders/transforms/meshes — textures were a blind
    /// spot, yet "Destroyed texture" GPU-validation spam + Chrome "aw snap" point
    /// at texture/sampler accumulation. A growing count under add/delete churn of
    /// textured materials / imported models signals a leak.
    pub fn resource_counts(&self) -> (usize, usize, usize) {
        (
            self.pool_textures.len(),
            self.cubemaps.len(),
            self.samplers.len(),
        )
    }

    /// Creates texture storage and GPU buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let samplers = SlotMap::with_key();
        let sampler_cache = HashMap::new();
        let sampler_address_modes = SecondaryMap::new();

        let texture_transforms_gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Texture Transforms"),
                TEXTURE_TRANSFORMS_INITIAL_CAPACITY * TEXTURE_TRANSFORMS_BYTE_SIZE,
                *TEXTURE_TRANSFORM_BUFFER_USAGE,
            )
            .into(),
        )?;
        let mut texture_transforms_buffer = DynamicUniformBuffer::new(
            TEXTURE_TRANSFORMS_INITIAL_CAPACITY,
            TEXTURE_TRANSFORMS_BYTE_SIZE,
            None,
            Some("Texture Transforms".to_string()),
        );

        let mut texture_transforms = SlotMap::with_key();

        let texture_transform_identity_offset = {
            let transform = TextureTransform::identity();
            let key = texture_transforms.insert(());

            texture_transforms_buffer.update(key, &transform.as_gpu_bytes());

            texture_transforms_buffer
                .offset(key)
                .expect("just inserted key must have offset")
        };

        Ok(Self {
            pool: TexturePool::new(),
            pool_sampler_set: IndexSet::new(),
            pool_textures: SlotMap::with_key(),
            cubemaps: SlotMap::with_key(),
            texture_transforms,
            texture_transforms_buffer,
            texture_transforms_gpu_buffer,
            texture_transforms_gpu_dirty: true,
            texture_transform_identity_offset,
            samplers,
            sampler_cache,
            sampler_address_modes,
            texture_transforms_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "Texture Transforms",
            ),
            sampler_pool_dirty: false,
        })
    }

    /// Mapped-ring upload telemetry for the texture transforms buffer.
    pub fn texture_transforms_upload_stats(
        &self,
    ) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.texture_transforms_uploader.stats()
    }

    /// Adds an image to the texture pool and returns its key.
    pub fn add_image(
        &mut self,
        image_data: ImageData,
        texture_format: TextureFormat,
        sampler_key: SamplerKey,
        color: TextureColorInfo,
    ) -> Result<TextureKey> {
        let key = self.pool_textures.try_insert_with_key(|key| {
            self.pool.add_image(key, image_data, texture_format, color);
            self.pool
                .entry(key)
                .ok_or(AwsmTextureError::TextureNotFound(key))
        })?;

        // `add_image` always flips the pool-array `gpu_dirty` bit, so
        // a *new* sampler insertion here will get picked up by the
        // standard `finalize_gpu_textures` rebuild path regardless of
        // `sampler_pool_dirty`. We still flip the flag so the
        // bookkeeping stays consistent with `ensure_sampler_in_pool`
        // — and so the rebuild gate is correct even if a future
        // change to `pool.add_image` ever skips the array dirty flag.
        if self.pool_sampler_set.insert(sampler_key) {
            self.sampler_pool_dirty = true;
        }

        Ok(key)
    }

    /// Adds a texture from raw RGBA8 bytes by packing them through a synchronous
    /// `OffscreenCanvas` → `ImageBitmap` round-trip and inserting via the
    /// standard pool path. Caller is responsible for triggering
    /// `AwsmRenderer::finalize_gpu_textures()` after a batch of additions so
    /// the new bitmaps actually upload to the GPU.
    pub fn add_image_rgba_raw(
        &mut self,
        rgba_bytes: &[u8],
        width: u32,
        height: u32,
        sampler_key: SamplerKey,
        color: TextureColorInfo,
    ) -> Result<TextureKey> {
        use wasm_bindgen::JsCast;
        let expected_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| {
                AwsmTextureError::ImageBitmapCreate(format!(
                    "rgba dims overflow: width={width} height={height}"
                ))
            })?;
        if rgba_bytes.len() != expected_len {
            return Err(AwsmTextureError::ImageBitmapCreate(format!(
                "rgba length mismatch: got {} bytes, want {expected_len} (width={width} height={height})",
                rgba_bytes.len()
            )));
        }

        let canvas = web_sys::OffscreenCanvas::new(width, height)
            .map_err(|e| AwsmTextureError::ImageBitmapCreate(format!("{e:?}")))?;
        let ctx_obj = canvas
            .get_context("2d")
            .map_err(|e| AwsmTextureError::ImageBitmapCreate(format!("get_context: {e:?}")))?
            .ok_or_else(|| {
                AwsmTextureError::ImageBitmapCreate("2d context unavailable".to_string())
            })?;
        let ctx: web_sys::OffscreenCanvasRenderingContext2d = ctx_obj.dyn_into().map_err(|_| {
            AwsmTextureError::ImageBitmapCreate("cast OffscreenCanvas 2d context".to_string())
        })?;

        // The FFI binding accepts `Clamped<&[u8]>`; web-sys will copy the
        // slice into a Wasm-side Uint8ClampedArray when constructing the
        // ImageData.
        let image_data = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
            wasm_bindgen::Clamped(rgba_bytes),
            width,
            height,
        )
        .map_err(|e| AwsmTextureError::ImageBitmapCreate(format!("ImageData::new: {e:?}")))?;
        ctx.put_image_data(&image_data, 0, 0)
            .map_err(|e| AwsmTextureError::ImageBitmapCreate(format!("put_image_data: {e:?}")))?;
        let bitmap = canvas
            .transfer_to_image_bitmap()
            .map_err(|e| AwsmTextureError::ImageBitmapCreate(format!("transfer: {e:?}")))?;

        self.add_image(
            ImageData::Bitmap {
                image: bitmap,
                options: None,
            },
            TextureFormat::Rgba8unorm,
            sampler_key,
            color,
        )
    }

    /// Inserts a texture transform and returns its key.
    pub fn insert_texture_transform(
        &mut self,
        transform: &TextureTransform,
    ) -> TextureTransformKey {
        let key = self.texture_transforms.insert(());
        self.update_texture_transform(key, transform);
        key
    }
    /// Updates an existing texture transform.
    pub fn update_texture_transform(
        &mut self,
        key: TextureTransformKey,
        transform: &TextureTransform,
    ) {
        let bytes = transform.as_gpu_bytes();
        self.texture_transforms_buffer.update(key, &bytes);
        self.texture_transforms_gpu_dirty = true;
    }

    /// Removes a texture transform.
    pub fn remove_texture_transform(&mut self, key: TextureTransformKey) {
        self.texture_transforms_buffer.remove(key);
        self.texture_transforms_gpu_dirty = true;
    }

    /// Returns the byte offset for a texture transform.
    pub fn get_texture_transform_offset(&self, key: TextureTransformKey) -> Option<usize> {
        self.texture_transforms_buffer.offset(key)
    }

    /// Returns the slot index for a texture transform.
    pub fn get_texture_transform_slot_index(&self, key: TextureTransformKey) -> Option<usize> {
        self.texture_transforms_buffer.slot_index(key)
    }

    /// Inserts a cubemap texture and returns its key.
    pub fn insert_cubemap(&mut self, texture: web_sys::GpuTexture) -> CubemapTextureKey {
        self.cubemaps.insert(texture)
    }

    /// Returns a cubemap texture by key.
    pub fn get_cubemap(&self, key: CubemapTextureKey) -> Result<&web_sys::GpuTexture> {
        self.cubemaps
            .get(key)
            .ok_or(AwsmTextureError::CubemapTextureNotFound(key))
    }

    async fn write_gpu_texture_pool(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
    ) -> Result<bool> {
        let _maybe_span_guard = if logging.render_timings.sub_frame() {
            Some(tracing::span!(tracing::Level::INFO, "Textures GPU write").entered())
        } else {
            None
        };

        self.pool.write_gpu(gpu).await.map_err(|e| e.into())
    }

    /// Writes texture transform data to the GPU if dirty.
    pub fn write_texture_transforms_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        if self.texture_transforms_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Texture Transforms GPU write").entered())
            } else {
                None
            };

            let mut resized = false;
            if let Some(new_size) = self.texture_transforms_buffer.take_gpu_needs_resize() {
                self.texture_transforms_gpu_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(
                        Some("Texture Transforms"),
                        new_size,
                        *TEXTURE_TRANSFORM_BUFFER_USAGE,
                    )
                    .into(),
                )?;

                bind_groups.mark_create(BindGroupCreate::TextureTransformsResize);
                resized = true;
            }

            if resized {
                self.texture_transforms_buffer.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.texture_transforms_gpu_buffer,
                    None,
                    self.texture_transforms_buffer.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let ranges = self.texture_transforms_buffer.take_dirty_ranges();
                self.texture_transforms_uploader.write_dirty_ranges(
                    gpu,
                    &self.texture_transforms_gpu_buffer,
                    self.texture_transforms_buffer.raw_slice().len(),
                    self.texture_transforms_buffer.raw_slice(),
                    &ranges,
                )?;
            }

            self.texture_transforms_gpu_dirty = false;
        }
        Ok(())
    }

    /// Removes a texture from the pool + slotmap. Returns `true` if the
    /// key existed; `false` if it was already gone. The pool recycles
    /// the freed layer slot for the next matching add — see
    /// [`TexturePool::remove`] for the invariants. Callers are
    /// responsible for ensuring no live material still binds this key
    /// (the renderer doesn't trace texture → material refs).
    pub fn remove(&mut self, key: TextureKey) -> bool {
        if self.pool.remove(key).is_some() {
            self.pool_textures.remove(key);
            true
        } else {
            false
        }
    }

    /// Returns pool entry info for a texture key.
    pub fn get_entry(&self, key: TextureKey) -> Result<&TexturePoolEntryInfo<TextureKey>> {
        self.pool_textures
            .get(key)
            .ok_or(AwsmTextureError::TextureNotFound(key))
    }

    /// Returns a sampler key, inserting if missing.
    pub fn get_sampler_key(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        cache_key: SamplerCacheKey,
    ) -> Result<SamplerKey> {
        if let Some(sampler_key) = self.sampler_cache.get(&cache_key) {
            return Ok(*sampler_key);
        }

        create_sampler_key(
            gpu,
            cache_key,
            &mut self.samplers,
            &mut self.sampler_cache,
            &mut self.sampler_address_modes,
        )
    }

    /// Register `sampler_key` in `pool_sampler_set` if it's not already
    /// there, marking the sampler-pool dirty bit so the next
    /// `finalize_gpu_textures` call rebuilds the texture-pool bind
    /// group + dependent pipeline layouts. Returns `true` when the
    /// insertion was new (and therefore a rebuild is required), `false`
    /// when the sampler was already in the pool.
    ///
    /// `add_image` already inserts the sampler that was passed at upload
    /// time, but a sampler that's only ever bound to a *cache-hit*
    /// texture (the editor's MaterialDef override path resolving a
    /// renderer-gltf-seeded `TextureKey`) wouldn't reach `add_image` —
    /// it'd silently fail `sampler_index` lookup at draw time and the
    /// shader would `SkipTexture` (rendering the material's base-color
    /// factor alone, i.e. pure white for a `[1,1,1,1]` default).
    ///
    /// Call this from any path that binds an existing `TextureKey` to
    /// a new sampler that wasn't previously in the pool, then ensure
    /// `AwsmRenderer::finalize_gpu_textures()` runs at the batch end
    /// — both the editor (`instance_batcher`, `particles_sync`) and
    /// the glTF populate path already do so unconditionally, which is
    /// why this returns `bool` rather than triggering a rebuild
    /// itself: the rebuild is async + expensive and should stay
    /// batched.
    pub fn ensure_sampler_in_pool(&mut self, sampler_key: SamplerKey) -> bool {
        let inserted = self.pool_sampler_set.insert(sampler_key);
        if inserted {
            self.sampler_pool_dirty = true;
        }
        inserted
    }

    /// Consume the sampler-pool dirty bit. `finalize_gpu_textures`
    /// ORs the returned value with the pool-write dirty bit when
    /// deciding whether to rebuild material bind groups + pipeline
    /// layouts.
    pub(crate) fn take_sampler_pool_dirty(&mut self) -> bool {
        std::mem::take(&mut self.sampler_pool_dirty)
    }

    /// Returns a sampler by key.
    pub fn get_sampler(&self, key: SamplerKey) -> Result<&web_sys::GpuSampler> {
        self.samplers
            .get(key)
            .ok_or(AwsmTextureError::SamplerNotFound(key))
    }

    /// Returns cached sampler address modes.
    pub fn sampler_address_modes(
        &self,
        key: SamplerKey,
    ) -> (Option<AddressMode>, Option<AddressMode>) {
        self.sampler_address_modes
            .get(key)
            .copied()
            .unwrap_or((None, None))
    }
}

fn create_sampler_key(
    gpu: &AwsmRendererWebGpu,
    cache_key: SamplerCacheKey,
    samplers: &mut SlotMap<SamplerKey, web_sys::GpuSampler>,
    sampler_cache: &mut HashMap<SamplerCacheKey, SamplerKey>,
    sampler_address_modes: &mut SecondaryMap<
        SamplerKey,
        (Option<AddressMode>, Option<AddressMode>),
    >,
) -> Result<SamplerKey> {
    let descriptor = SamplerDescriptor {
        label: None,
        address_mode_u: cache_key.address_mode_u,
        address_mode_v: cache_key.address_mode_v,
        address_mode_w: cache_key.address_mode_w,
        compare: cache_key.compare,
        lod_min_clamp: cache_key.lod_min_clamp.map(|x| x.into_inner()),
        lod_max_clamp: cache_key.lod_max_clamp.map(|x| x.into_inner()),
        max_anisotropy: cache_key.max_anisotropy,
        mag_filter: cache_key.mag_filter,
        min_filter: cache_key.min_filter,
        mipmap_filter: cache_key.mipmap_filter,
    };

    // tracing::info!("address_mode_u: {address_mode_u:?}, address_mode_v: {address_mode_v:?}, address_mode_w: {address_mode_w:?}, compare: {compare:?}, lod_min_clamp: {lod_min_clamp:?}, lod_max_clamp: {lod_max_clamp:?}, max_anisotropy: {max_anisotropy:?}, mag_filter: {mag_filter:?}, min_filter: {min_filter:?}, mipmap_filter: {mipmap_filter:?}",
    //     address_mode_u = cache_key.address_mode_u,
    //     address_mode_v = cache_key.address_mode_v,
    //     address_mode_w = cache_key.address_mode_w,
    //     compare = cache_key.compare,
    //     lod_min_clamp = cache_key.lod_min_clamp,
    //     lod_max_clamp = cache_key.lod_max_clamp,
    //     max_anisotropy = cache_key.max_anisotropy,
    //     mag_filter = cache_key.mag_filter,
    //     min_filter = cache_key.min_filter,
    //     mipmap_filter = cache_key.mipmap_filter,
    // );

    let sampler = gpu.create_sampler(Some(&descriptor.into()));

    let key = samplers.insert(sampler);
    let address_mode_u = cache_key.address_mode_u;
    let address_mode_v = cache_key.address_mode_v;
    sampler_cache.insert(cache_key, key);
    // Persist the original (U,V) wrap modes so that shader-side helpers can reproduce the
    sampler_address_modes.insert(key, (address_mode_u, address_mode_v));

    Ok(key)
}

// `TextureKey`, `TextureTransformKey`, `SamplerKey` moved to
// `awsm-renderer-core::keys` so the `awsm-materials` crate can reference
// them without depending on `awsm-renderer`. Re-exported here for backward
// compat with existing callers that import via `awsm_renderer`.
pub use awsm_renderer_core::keys::{SamplerKey, TextureKey, TextureTransformKey};

impl awsm_materials::TextureContext for Textures {
    fn pool_array_by_index(
        &self,
        index: usize,
    ) -> Option<&awsm_renderer_core::texture::texture_pool::TexturePoolArray<TextureKey>> {
        self.pool.array_by_index(index)
    }

    fn texture_entry(&self, key: TextureKey) -> Option<&TexturePoolEntryInfo<TextureKey>> {
        self.pool_textures.get(key)
    }

    fn sampler_index(&self, key: SamplerKey) -> Option<u32> {
        self.pool_sampler_set.get_index_of(&key).map(|i| i as u32)
    }

    fn sampler_address_modes(&self, key: SamplerKey) -> (Option<AddressMode>, Option<AddressMode>) {
        self.sampler_address_modes
            .get(key)
            .copied()
            .unwrap_or((None, None))
    }

    fn texture_transform_offset(&self, key: TextureTransformKey) -> Option<usize> {
        self.get_texture_transform_offset(key)
    }

    fn texture_transform_identity_offset(&self) -> usize {
        self.texture_transform_identity_offset
    }
}

new_key_type! {
    /// Opaque key for cubemap textures.
    pub struct CubemapTextureKey;
}

/// Result type for texture operations.
pub type Result<T> = std::result::Result<T, AwsmTextureError>;

/// Texture-related errors.
#[derive(Error, Debug)]
pub enum AwsmTextureError {
    #[error("[texture] {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[texture] pool failure")]
    Pool,

    #[error("[texture] sampler not found: {0:?}")]
    SamplerNotFound(SamplerKey),

    #[error("[texture] texture not found: {0:?}")]
    TextureNotFound(TextureKey),

    #[error("[texture] subemap texture not found: {0:?}")]
    CubemapTextureNotFound(CubemapTextureKey),

    #[error("[texture] no clamp sampler found in mega-texture")]
    NoClampSamplerInMegaTexture,

    #[error("[texture] runtime image bitmap creation failed: {0}")]
    ImageBitmapCreate(String),
}
