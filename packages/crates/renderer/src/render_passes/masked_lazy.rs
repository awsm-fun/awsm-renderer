//! Lazy compile of the built-in **masked** (alpha-tested) variants between
//! commits.
//!
//! `commit_load` compiles the masked geometry + masked shadow pipelines of
//! every built-in family that has an alpha-tested material, and awaits them
//! (`finalize_gpu_textures`), so a freshly loaded scene is cut out on its first
//! frame. This module covers the other way a family can come to need them: a
//! material turned alpha-tested on a running scene — an `update_material`, or
//! a new masked material assigned with `set_mesh_material` — with no commit
//! afterwards. Until its family's variants exist such a mesh draws through the
//! plain (solid) pipelines, un-cut; the render loop notices, compiles the
//! variants in the background and installs them when they resolve, the same
//! kick/poll shape as the line and decal pipelines.
//!
//! Geometry and shadow variants are issued and installed as ONE batch, so a
//! mesh never spends frames cut out while still casting a solid shadow.

use std::future::Future;
use std::pin::Pin;

use awsm_renderer_materials::MaterialShaderId;
use wasm_bindgen::JsValue;

use crate::error::Result;
use crate::pipelines::render_pipeline::{
    RenderPipelineCacheKey, RenderPipelineKey, RenderPipelines, RenderPipelinesPrep,
};
use crate::render_passes::geometry::masked_pipeline::{
    GeometryMaskedPipelines, MaskedPipelineKeyId, MaskedVariant,
};
use crate::render_passes::shadow_masked::pipeline::{
    MaskedShadowPipelineKeyId, MaskedShadowVariant, ShadowMaskedPipelines,
};
use crate::render_passes::RenderPassInitContext;
use crate::AwsmRenderer;

/// State of the lazy masked-variant compile. Lives on the renderer.
#[derive(Default)]
pub(crate) struct MaskedLazyCompile {
    inflight: Option<Inflight>,
    /// What a lazy compile was last rejected for. Latched so a variant the
    /// driver refuses is not re-issued every frame; forgotten as soon as the
    /// pools are rebuilt (their epoch moves), which is also when a commit
    /// retries it and reports the error properly.
    failed: Option<Attempt>,
}

/// What one lazy compile was issued for.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Attempt {
    geometry_epoch: u64,
    shadow_epoch: u64,
    geometry_targets: crate::render_passes::geometry::pipeline::GeometryTargets,
    families: Vec<MaterialShaderId>,
}

struct Inflight {
    attempt: Attempt,
    /// Every pipeline of the batch, geometry first then shadow; `Some` where
    /// the render-pipeline pool already had it compiled.
    resolved: Vec<Option<RenderPipelineKey>>,
    /// Indices into `resolved` of the pipelines being compiled, in the order
    /// `prep` / `joined` hold them.
    pending: Vec<usize>,
    prep: RenderPipelinesPrep,
    #[allow(clippy::type_complexity)]
    joined: Pin<
        Box<dyn Future<Output = Vec<std::result::Result<web_sys::GpuRenderPipeline, JsValue>>>>,
    >,
    geometry_slots: Vec<MaskedPipelineKeyId>,
    shadow_slots: Vec<MaskedShadowPipelineKeyId>,
}

impl AwsmRenderer {
    /// Render-loop auto-drive (step 1, sync, non-blocking): when a built-in
    /// family has an alpha-tested material but no masked variants — see the
    /// module docs — issue their compiles. A couple of integer checks when
    /// there is nothing to do, which is every frame of a scene with no
    /// cutout, and every frame after the variants exist.
    pub(crate) fn kick_masked_variants_compile(&mut self) -> Result<()> {
        if self.masked_lazy.inflight.is_some() || self.materials.masked_builtin().is_empty() {
            return Ok(());
        }
        let geometry_targets = crate::render_passes::geometry::pipeline::GeometryTargets::from_aa(
            &self.anti_aliasing,
            &self.render_textures.formats,
        );
        let geometry_pool = &self.render_passes.geometry.masked_pipelines;
        let shadow_pool = &self.render_passes.shadow_masked.pipelines;

        let mut geometry_variants = Vec::new();
        let mut shadow_variants = Vec::new();
        let mut families = Vec::new();
        for (shader_id, base) in self.materials.masked_builtin().families() {
            let needs_geometry = !geometry_pool.has_variant(geometry_targets, shader_id);
            let needs_shadow = !shadow_pool.has_variant(shader_id);
            if needs_geometry {
                geometry_variants.push(MaskedVariant {
                    shader_id,
                    base,
                    dynamic_alpha: None,
                });
            }
            if needs_shadow {
                shadow_variants.push(MaskedShadowVariant {
                    shader_id,
                    base,
                    dynamic_alpha: None,
                });
            }
            if needs_geometry || needs_shadow {
                families.push(shader_id);
            }
        }
        if families.is_empty() {
            return Ok(());
        }
        let attempt = Attempt {
            geometry_epoch: geometry_pool.epoch(),
            shadow_epoch: shadow_pool.epoch(),
            geometry_targets,
            families,
        };
        if self.masked_lazy.failed.as_ref() == Some(&attempt) {
            return Ok(());
        }
        self.masked_lazy.failed = None;

        let ctx = RenderPassInitContext {
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
        // Disjoint field borrows: `ctx` holds the caches; `render_passes` is
        // not part of it.
        let geometry_pool = &self.render_passes.geometry.masked_pipelines;
        let shadow_pool = &self.render_passes.shadow_masked.pipelines;

        // Shader modules are created sync and not validated here (never block
        // the render path on a compile); a broken module surfaces as a rejected
        // pipeline in `poll_masked_variants_compile`.
        let geometry_shaders = GeometryMaskedPipelines::shader_cache_keys(
            &ctx,
            &self.render_passes.geometry.masked_bind_group,
            &geometry_variants,
        );
        let shadow_shaders = ShadowMaskedPipelines::shader_cache_keys(
            &self.render_passes.shadow_masked.bind_group,
            &shadow_variants,
        );
        let geometry_shader_count = geometry_shaders.len();
        let shader_keys = ctx.shaders.ensure_keys_sync_skip_validate(
            ctx.gpu,
            geometry_shaders.into_iter().chain(shadow_shaders),
        )?;
        let (geometry_shader_keys, shadow_shader_keys) =
            shader_keys.split_at(geometry_shader_count);

        let (mut cache_keys, geometry_slots) =
            geometry_pool.pipeline_cache_keys(&ctx, &geometry_variants, geometry_shader_keys)?;
        let (shadow_cache_keys, shadow_slots) =
            shadow_pool.pipeline_cache_keys(&ctx, &shadow_variants, shadow_shader_keys);
        cache_keys.extend(shadow_cache_keys);

        // Only what the render-pipeline pool does not already hold is
        // compiled (a variant dropped by a pool rebuild is usually still
        // cached there).
        let resolved: Vec<Option<RenderPipelineKey>> = cache_keys
            .iter()
            .map(|cache_key| ctx.pipelines.render.get_cached_key(cache_key))
            .collect();
        let (pending, pending_cache_keys): (Vec<usize>, Vec<RenderPipelineCacheKey>) = cache_keys
            .into_iter()
            .enumerate()
            .filter(|(index, _)| resolved[*index].is_none())
            .unzip();
        let mut prepped = RenderPipelines::ensure_keys_prepare(
            ctx.gpu,
            ctx.shaders,
            ctx.pipeline_layouts,
            pending_cache_keys,
        )?;
        let promises = std::mem::take(&mut prepped.promises);
        self.masked_lazy.inflight = Some(Inflight {
            attempt,
            resolved,
            pending,
            prep: prepped.prep,
            joined: Box::pin(futures::future::join_all(promises)),
            geometry_slots,
            shadow_slots,
        });
        Ok(())
    }

    /// Render-loop auto-drive (step 2, sync, non-blocking): install the lazy
    /// masked variants once every one of them has resolved. No-op while
    /// nothing is in flight or the compiles are still running.
    ///
    /// Never fails the frame: a rejected compile is logged once and the
    /// affected meshes keep drawing un-cut (as they did while it compiled).
    pub(crate) fn poll_masked_variants_compile(&mut self) {
        use futures::FutureExt;
        let ready = match self.masked_lazy.inflight.as_mut() {
            Some(inflight) => inflight.joined.as_mut().now_or_never(),
            None => return,
        };
        let Some(results) = ready else {
            return;
        };
        let Some(inflight) = self.masked_lazy.inflight.take() else {
            return;
        };
        let Inflight {
            attempt,
            mut resolved,
            pending,
            prep,
            joined: _,
            geometry_slots,
            shadow_slots,
        } = inflight;

        // The pipelines themselves are content-addressed, so they go into the
        // render-pipeline pool whatever happened meanwhile…
        let keys = match self.pipelines.render.ensure_keys_install(prep, results) {
            Ok(keys) => keys,
            Err(err) => {
                tracing::warn!(
                    "masked (alpha-tested) variants for {:?} failed to compile; \
                     those materials keep drawing un-cut: {err}",
                    attempt.families
                );
                self.masked_lazy.failed = Some(attempt);
                return;
            }
        };
        for (index, key) in pending.into_iter().zip(keys) {
            resolved[index] = Some(key);
        }
        // …but the variant pools only take them if they still have the layout
        // these were built against. A pool rebuilt since (texture-pool growth,
        // an MSAA flip — both recompile at their own commit) has moved on; the
        // next kick re-issues whatever it is still missing.
        let geometry_pool = &mut self.render_passes.geometry.masked_pipelines;
        let shadow_pool = &mut self.render_passes.shadow_masked.pipelines;
        if geometry_pool.epoch() != attempt.geometry_epoch
            || shadow_pool.epoch() != attempt.shadow_epoch
        {
            return;
        }
        let Some(mut keys) = resolved.into_iter().collect::<Option<Vec<_>>>() else {
            return;
        };
        let shadow_keys = keys.split_off(geometry_slots.len());
        geometry_pool.insert_resolved(geometry_slots, keys);
        shadow_pool.insert_resolved(shadow_slots, shadow_keys);
    }
}
