//! Render entry points and render context.

use awsm_renderer_core::command::{
    color::Color,
    render_pass::{
        ColorAttachment, DepthStencilAttachment, RenderPassDescriptor, RenderPassEncoder,
    },
    CommandEncoder, LoadOp, StoreOp,
};
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use awsm_renderer_core::texture::blit::blit_tex;

use crate::anti_alias::AntiAliasing;
use crate::bind_groups::{BindGroupCreate, BindGroupRecreateContext, BindGroups};
use crate::error::{AwsmError, Result};
use crate::instances::Instances;
use crate::materials::Materials;
use crate::meshes::Meshes;
use crate::pipelines::Pipelines;
use crate::post_process::PostProcessing;
use crate::render_passes::RenderPasses;
use crate::render_textures::{RenderTextureViews, RenderTextures};
use crate::scene_spatial::SceneSpatial;
use crate::transforms::Transforms;
use crate::{AwsmRenderer, AwsmRendererLogging};

/// Optional callbacks around render passes.
#[derive(Default)]
pub struct RenderHooks {
    /// Runs before per-frame CPU->GPU writes and pass execution.
    pub pre_render: Option<Box<dyn Fn(&mut AwsmRenderer) -> Result<()>>>,
    /// Runs before geometry/light/material passes (advanced setup use-cases).
    pub first_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after geometry passes and before light culling/material opaque shading.
    ///
    /// Use this for advanced visibility-buffer extensions that need to contribute additional
    /// world-space opaque geometry.
    pub after_geometry_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after opaque->transparent blit and before world transparent materials.
    pub before_transparent_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after world transparent materials and before HUD transparent rendering.
    pub after_transparent_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after display pass and before command submission.
    pub last_pass: Option<Box<dyn Fn(&RenderContext) -> Result<()>>>,
    /// Runs after command submission.
    pub post_render: Option<Box<dyn Fn(&mut AwsmRenderer) -> Result<()>>>,
}

impl AwsmRenderer {
    // this should only be called once per frame
    // the various underlying raw data can be updated on their own cadence
    // or just call .update_all() right before .render() for convenience
    /// Executes a full render with optional hooks.
    pub fn render(&mut self, hooks: Option<&RenderHooks>) -> Result<()> {
        // Per-frame pre-amble: drain the pipeline scheduler's resolved
        // compile futures. Transitions (Pending → Ready / Failed) are
        // applied here; bucket-entries cache invalidations + per-pass
        // typed-accessor cache refreshes happen synchronously off the
        // status events. This is the load-bearing "transitions happen
        // between frames, not mid-frame" invariant from
        // https://github.com/dakom/awsm-renderer/pull/99 § Scheduler driving and
        // transition timing.
        let applied = self.poll_pipeline_scheduler();
        if applied > 0 {
            tracing::debug!(
                target: "awsm_renderer::pipeline_readiness",
                "render-preamble drain: {} transitions applied",
                applied
            );
        }
        // The status events themselves are drained by frontends via
        // drain_pipeline_status_events. The renderer-internal side
        // effects (cache rebuilds) are applied directly inside the
        // scheduler's apply_resolution path — see
        // pipeline_scheduler/mod.rs for the wiring.

        // Fat-line pipelines are render pipelines, not compute, so they
        // sit outside the scheduler's `poll_pipeline_scheduler` pump
        // above. Drive their lazy compile here on the same per-frame
        // cadence: `kick_compile` issues the `createRenderPipelineAsync`
        // promises the first frame after a line is registered (sync,
        // non-blocking); `poll_compile` installs them once resolved.
        // Both are ~free no-ops when no line primitive exists, so
        // projects that never use lines pay nothing.
        self.lines.kick_compile(
            &self.gpu,
            &self.shaders,
            &mut self.bind_group_layouts,
            &mut self.pipeline_layouts,
            &self.render_textures.formats,
        )?;
        self.lines.poll_compile(&mut self.pipelines.render)?;

        // HUD meshes (editor gizmos, in-game HUD primitives) draw
        // through the transparent pipeline, which is per-mesh and
        // texture-pool-shape-coupled. Unlike world transparents — whose
        // keys are resolved on the scene-load path (gltf populate /
        // texture finalize) — HUD overlays are inserted live, *after*
        // boot's one-shot prewarm, so without this they'd reference no
        // (or a stale) pipeline and fall back to the grey error pipeline.
        // `kick_hud_resolve` re-resolves only `mesh.hud` meshes whenever
        // a HUD mesh appears or the texture-pool / MSAA shape changes;
        // `poll_hud_resolve` installs the resolved variants. Both
        // early-out on `!has_seen_hud()`, so non-HUD projects pay nothing.
        self.kick_hud_resolve()?;
        self.poll_hud_resolve()?;

        if let Some(hook) = hooks.and_then(|h| h.pre_render.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                    Some(tracing::span!(tracing::Level::INFO, "PreRender Hook").entered())
                } else {
                    None
                };
                hook(self)?;
            }
        }

        // Outermost frame span — fires on any non-Off tier so the
        // shipping build still produces one `performance.measure`
        // per frame. Everything *inside* `render()` is gated on
        // `.sub_frame()`.
        let _maybe_span_guard = if self.logging.render_timings.enabled() {
            Some(tracing::span!(tracing::Level::INFO, "Render").entered())
        } else {
            None
        };

        self.render_textures.next_frame();

        // Ingest any coverage snapshot that a prior frame's
        // `mapAsync` task resolved into
        // `coverage_readback_state.pending_snapshot`. The producer
        // pass dispatched N frames ago; consumers (skin-skip /
        // material LOD) see this-frame counts on the very next
        // frame this hook runs. No-op when nothing has been
        // resolved — including when `features.coverage_lod` is off,
        // since no producer was scheduled.
        let pending_snapshot = self
            .coverage_readback_state
            .lock()
            .unwrap()
            .pending_snapshot
            .take();
        if let Some(snapshot) = pending_snapshot {
            self.coverage.ingest(snapshot, self.frame_index);
        }

        // Ingest any MSAA edge-overflow snapshot resolved on a prior
        // frame's `mapAsync`. When `edge_overflow_count > 0`, the
        // classify pass dropped that many edges past `MAX_EDGE_BUDGET`
        // and those pixels shaded with sample-0 instead of full MSAA.
        // Auto-grow: double the budget so the next frame has headroom.
        // No-op when MSAA is off (no edge buffers, no producer
        // scheduled), or when the prior `mapAsync` returned 0.
        let pending_edge_overflow = self
            .edge_overflow_readback_state
            .lock()
            .unwrap()
            .pending_overflow_count
            .take();
        if let Some((edge_count, overflow_count)) = pending_edge_overflow {
            if overflow_count > 0 {
                let current_budget = self
                    .material_edge_buffers
                    .as_ref()
                    .map(|eb| eb.max_edge_budget)
                    .unwrap_or(0);
                if current_budget > 0 {
                    // Double, clamped at u32::MAX/2 so the *2 in a
                    // future overflow can't wrap. Realistic overflow
                    // counts stay well below this — the clamp is just
                    // for defense.
                    let new_budget = current_budget.saturating_mul(2).min(u32::MAX / 2);
                    if new_budget > current_budget {
                        match self.set_max_edge_budget(new_budget) {
                            Ok(true) => {
                                tracing::info!(
                                    target: "awsm_renderer::edge_resolve",
                                    edge_count,
                                    overflow_count,
                                    new_budget,
                                    "edge-overflow auto-grow: doubled MAX_EDGE_BUDGET \
                                     ({current_budget} -> {new_budget}) to absorb \
                                     {overflow_count} dropped edges from the prior frame",
                                );
                            }
                            Ok(false) => {}
                            Err(e) => {
                                tracing::warn!(
                                    target: "awsm_renderer::edge_resolve",
                                    "edge-overflow auto-grow failed: {e:?}; pathological \
                                     scenes will continue dropping edges (degraded MSAA, not \
                                     a crash)",
                                );
                            }
                        }
                    }
                }
                // Surface the one-shot warn helper too, so
                // tracing-subscribers without the auto-grow info-line
                // filter still see the overflow signal.
                crate::render_passes::material_opaque::edge_buffers::note_edge_overflow_observed(
                    overflow_count,
                    current_budget,
                );
            }
        }

        // Ingest GPU light-culling froxel-overflow readback. Same
        // shape as the edge-overflow ingest above. When the cull
        // shader bumped a froxel's count past
        // `max_per_froxel_capacity` (so subsequent lights for that
        // froxel were dropped), double the budget for next frame.
        let pending_froxel_overflow = self
            .froxel_overflow_readback_state
            .lock()
            .unwrap()
            .pending_overflow_count
            .take();
        if let Some(overflow_count) = pending_froxel_overflow {
            if overflow_count > 0 {
                let current_capacity = self.light_culling_buffers.max_per_froxel_capacity;
                let new_capacity = current_capacity.saturating_mul(2).min(u32::MAX / 2);
                if new_capacity > current_capacity {
                    match self.set_max_per_froxel_capacity(new_capacity) {
                        Ok(true) => {
                            tracing::info!(
                                target: "awsm_renderer::light_culling",
                                overflow_count,
                                new_capacity,
                                "light-culling overflow auto-grow: doubled \
                                 max_per_froxel_capacity ({current_capacity} -> \
                                 {new_capacity}) to absorb {overflow_count} dropped \
                                 light indices from the prior frame",
                            );
                        }
                        Ok(false) => {}
                        Err(e) => {
                            tracing::warn!(
                                target: "awsm_renderer::light_culling",
                                "light-culling overflow auto-grow failed: {e:?}; \
                                 pathological scenes will continue dropping indices",
                            );
                        }
                    }
                }
            }
        }

        // Cheap-material LOD: re-route every mesh-with-a-cheap-variant
        // to the effective material's GPU offset based on the
        // just-ingested coverage. Idempotent — the
        // `last_effective_material` sidecar inside `Meshes` short-
        // circuits unchanged meshes, so steady-state writes are O(0)
        // even when every mesh has a cheap variant authored. Must
        // run BEFORE `meshes.meta.write_gpu` below so the patched
        // offsets land in the same upload as other per-frame meta
        // edits (light slice / shadow gate).
        self.meshes.refresh_cheap_material_routing(
            &self.materials,
            &self.coverage,
            self.default_cheap_material_pixel_threshold,
        )?;

        // Specialize-only pivot: route PBR/Toon materials (opaque and
        // transparent) to their per-feature-set variant buckets BEFORE the
        // material GPU write (so the resolved variant id lands in the
        // payload's first u32 in this same frame) and before classify
        // dispatch (which routes on it). Cheap no-op once the material set
        // settles (gated internally on `variants_dirty`).
        self.reconcile_material_variants()?;

        self.transforms
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.materials
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.instances
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.meshes
            .skins
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.meshes
            .morphs
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        // ── Light culling per-frame setup ────────────────────────
        //
        // `ensure_viewport` may recreate `light_culling_buffers.storage_buffer`
        // (the froxel storage), so it must land before the cull dispatch
        // writes the per-froxel slices into it for this frame.
        let (viewport_w_for_cull, viewport_h_for_cull) = self.gpu.current_context_texture_size()?;
        if self.light_culling_buffers.ensure_viewport(
            &self.gpu,
            viewport_w_for_cull,
            viewport_h_for_cull,
        )? {
            self.bind_groups
                .mark_create(BindGroupCreate::LightCullingFroxelsResize);
        }
        // Grow the per-2D-tile candidate capacity toward the live
        // punctual-light count. A tile column can't hold more candidates
        // than there are punctual lights, so this is a safe
        // non-overflowing bound — and it keeps the `tile_lights` buffer
        // small for low-light scenes (the common case). MUST run before
        // `write_params` so the `tile_light_capacity` written into
        // `cull_params` matches the (possibly resized) buffer.
        let live_punctual_for_cull = self.lights.iter_active_punctual().count() as u32;
        if self
            .light_culling_buffers
            .ensure_tile_light_capacity(&self.gpu, live_punctual_for_cull)?
        {
            self.bind_groups
                .mark_create(BindGroupCreate::LightCullingFroxelsResize);
        }
        let (z_near_for_cull, z_far_for_cull) =
            camera_near_far_from_projection(&self.camera.last_matrices);
        self.light_culling_buffers.write_params(
            &self.gpu,
            z_near_for_cull,
            z_far_for_cull,
            self.light_culling_debug_heatmap,
            self.debug_view_mode,
            self.debug_wireframe,
        )?;
        self.light_culling_buffers.reset_overflow(&self.gpu)?;

        // (Removed: the per-mesh light-slice GPU upload. All opaque
        // shading now reads the per-pixel froxel light list, so the
        // per-mesh slices in the storage-buffer head are no longer
        // consumed. `LightMeshBuckets` is still rebuilt elsewhere — it
        // feeds the shadow-receiver gate — but its slices aren't uploaded.)

        // Decals — upload per-decal data if anything changed since last
        // frame. Skipped entirely when the decals feature is off.
        if let Some(decals) = self.decals.as_mut() {
            decals.write_gpu(&self.gpu, &mut self.bind_groups)?;
        }
        self.meshes
            .meta
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.textures.write_texture_transforms_gpu(
            &self.logging,
            &self.gpu,
            &mut self.bind_groups,
        )?;
        self.meshes
            .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups)?;
        self.camera
            .write_gpu(&self.logging, &self.gpu, &self.bind_groups)?;
        // Live swap-chain size — read once per frame and threaded through
        // `RenderContext.viewport_size`. The value is fixed for the
        // duration of the frame (set by the surface configuration), so
        // every pass-level caller that previously re-read it
        // (`render_textures.views`, `update_camera`, the FrameGlobals
        // upload below) now consults the same cached pair. Each
        // `current_context_texture_size()` is a `getCurrentTexture().getSize()`
        // wasm↔JS hop — cheap individually but it stacked up.
        let viewport_size = self.gpu.current_context_texture_size()?;
        // FrameGlobals — renderer-wide per-frame uniform. Written after
        // Camera so it shares the same upload batch and lands before any
        // pass that reads it. Resolution comes from the live context
        // texture (matches what render_textures wants to be sized to).
        {
            self.frame_globals.write_gpu(
                &self.logging,
                &self.gpu,
                self.render_textures.frame_count(),
                [viewport_size.0, viewport_size.1],
            )?;
        }
        // Extras pool — flush any dirty bytes from BufferSlot
        // updates this frame. No-op when nothing's changed.
        self.extras_pool.write_gpu(&self.gpu)?;
        // Shadows must fit cascades + populate the descriptor buffer
        // *before* the lights buffer is packed — `Lights::write_gpu`
        // queries `shadow_index_for` per-light and bakes the result
        // into `LightPacked.row4.z`.
        self.shadows.write_gpu(
            &self.logging,
            &self.gpu,
            &self.bind_group_layouts,
            &mut self.bind_groups,
            &self.camera,
            &self.lights,
            &self.scene_spatial,
        )?;
        {
            let shadows = &self.shadows;
            self.lights
                .write_gpu(&self.logging, &self.gpu, &mut self.bind_groups, |key| {
                    shadows.descriptor_index_for_light(key)
                })?;
        }

        let render_texture_views = self.render_textures.views(
            &self.gpu,
            self.anti_aliasing.clone(),
            viewport_size,
            // T2.5: lazy opaque-mip-chain allocation. Once a
            // transmissive material registers, the flag is sticky
            // true and the texture grows to full mip count on the
            // next `views()`.
            self.materials.has_seen_transmission(),
            // T2.6: lazy HUD depth allocation. False until the first
            // HUD-flagged mesh enters the registry.
            self.meshes.has_seen_hud(),
        )?;

        if render_texture_views.views_recreated {
            self.bind_groups
                .mark_create(BindGroupCreate::TextureViewRecreate);
        }

        // Resize the HZB texture to match the live viewport. This
        // recreates the per-mip views, so the HZB bind groups must
        // also be rebuilt — the `TextureViewRecreate` event above
        // covers that since a viewport resize implies
        // `views_recreated == true`.
        // Skipped when `features.gpu_culling == false`.
        if let Some(hzb) = self.render_passes.hzb.as_mut() {
            if hzb.ensure_size(
                &self.gpu,
                render_texture_views.width,
                render_texture_views.height,
            )? {
                self.bind_groups
                    .mark_create(BindGroupCreate::TextureViewRecreate);
            }
        }

        // Tile counts are reused by both the opaque classify buckets
        // (here) and the decal classify buckets (below). Calculate
        // once so the two callers can't drift if the workgroup tile
        // size ever changes.
        let tile_x = render_texture_views.width.div_ceil(8);
        let tile_y = render_texture_views.height.div_ceil(8);
        let tile_count = tile_x.saturating_mul(tile_y);

        // Classify buckets are sized to fit the current viewport's
        // tile count. The grow-with-2x path keeps the reallocation
        // away from the steady-state per-frame work. Reset the header
        // every frame so the atomic counters start at 0.
        if self
            .material_classify_buffers
            .ensure_capacity(&self.gpu, tile_count)?
        {
            self.bind_groups
                .mark_create(BindGroupCreate::MaterialClassifyBuffersResize);
        }
        self.material_classify_buffers.reset_header(&self.gpu)?;

        // (Light-culling per-frame setup runs earlier, before the cull
        // dispatch writes into `light_culling_buffers.storage_buffer`.)

        // Build a snapshot of the active mesh count so we can size the
        // occlusion-cull buffers before bind groups are recreated.
        // Refining this to the actual opaque-renderable count requires
        // `collect_renderables` which runs later; this upper bound is
        // fine for capacity planning. Skipped when
        // `features.gpu_culling == false`.
        let occlusion_needed = self.meshes.len() as u32;
        if let Some(occlusion_buffers) = self.occlusion_buffers.as_mut() {
            if occlusion_buffers.ensure_capacity(&self.gpu, occlusion_needed)? {
                self.bind_groups
                    .mark_create(BindGroupCreate::OcclusionBuffersResize);
            }
        }

        // Decal classify buckets sized to viewport tile count.
        // Reuses the `tile_x` / `tile_y` calculated above.
        // Skipped when `features.decals == false`.
        if let Some(decal_classify_buffers) = self.decal_classify_buffers.as_mut() {
            if decal_classify_buffers.ensure_capacity(&self.gpu, tile_x, tile_y)? {
                self.bind_groups
                    .mark_create(BindGroupCreate::DecalClassifyBuffersResize);
            }
            // Per-tile atomic-count reset moved off the CPU upload path
            // — see `decal_classify_buffers.reset_counts(...)` below,
            // recorded into the command encoder just before the
            // material_decal pass. The original `gpu.write_buffer`
            // re-uploaded the *entire* bucket buffer every frame
            // (~17 MB at 4K, scaled with viewport).
        }

        // Ensure the compaction args buffer covers every mesh slot.
        // Skipped when `features.gpu_culling == false`.
        if let Some(compaction_buffers) = self.compaction_buffers.as_mut() {
            if compaction_buffers.ensure_capacity(&self.gpu, self.meshes.len() as u32)? {
                self.bind_groups
                    .mark_create(BindGroupCreate::CompactionBuffersResize);
            }
        }

        // GPU coverage producer: ensure the per-mesh counts buffer
        // covers every slot, then zero it for this frame so the
        // compute pass's atomicAdd starts clean. Sizing follows
        // the same `meshes.len()` upper bound as the compaction
        // args buffer; sparse meta-slot indices leave gaps that
        // stay at zero across frames (harmless — consumers treat
        // zero counts as "not visible last frame"). Skipped
        // entirely when `features.coverage_lod == false`.
        if let Some(coverage_buffers) = self.coverage_buffers.as_mut() {
            if coverage_buffers.ensure_capacity(&self.gpu, self.meshes.len() as u32)? {
                self.bind_groups
                    .mark_create(BindGroupCreate::CoverageBuffersResize);
            }
            // Per-frame zero of `counts_buffer` is recorded into the
            // frame's command encoder below (alongside the coverage
            // compute dispatch itself). Capacity sizing lands here so
            // any bind-group recreate event fires before the encoder
            // path consumes the new layout.
        }

        self.bind_groups.recreate(
            BindGroupRecreateContext {
                gpu: &self.gpu,
                render_texture_views: &render_texture_views,
                textures: &self.textures,
                materials: &self.materials,
                bind_group_layouts: &mut self.bind_group_layouts,
                meshes: &self.meshes,
                camera: &self.camera,
                frame_globals: &self.frame_globals,
                environment: &self.environment,
                lights: &self.lights,
                transforms: &self.transforms,
                instances: &self.instances,
                anti_aliasing: &self.anti_aliasing,
                shadows: &self.shadows,
                material_classify_buffers: &self.material_classify_buffers,
                material_bucket_lut: &self.material_bucket_lut,
                light_culling_buffers: &self.light_culling_buffers,
                material_edge_buffers: self.material_edge_buffers.as_ref(),
                material_edge_layout_uniform: self.material_edge_layout_uniform.as_ref(),
                extras_pool: &self.extras_pool,
                decals: self.decals.as_ref(),
                occlusion_buffers: self.occlusion_buffers.as_ref(),
                hzb_full_view: self
                    .render_passes
                    .hzb
                    .as_ref()
                    .map(|hzb| hzb.texture.view_all.clone()),
                decal_classify_buffers: self.decal_classify_buffers.as_ref(),
                compaction_buffers: self.compaction_buffers.as_ref(),
                coverage_buffers: self.coverage_buffers.as_ref(),
                features: &self.features,
                // Stage 5b-shadow: clone the prep pass's compact edge-shadow view
                // (owned, not a borrow) so this shared read doesn't conflict with
                // the `&mut self.render_passes` argument below. `None` unless prep
                // + MSAA. Bound at opaque group(0) binding 27 for cs_edge's EDGE read.
                prep_edge_shadow_view: self
                    .render_passes
                    .material_prep
                    .as_ref()
                    .and_then(|p| p.edge_shadow.as_ref())
                    .map(|b| b.sampled_view.clone()),
            },
            &mut self.render_passes,
            self.picker.as_mut(),
        )?;

        // Populate the pooled renderable lists BEFORE building the
        // RenderContext — `collect_renderables` takes `&mut self` to
        // clear-and-extend the pool's Vecs in place, while ctx holds
        // immutable references into `self`.
        self.collect_renderables()?;
        let renderables = self.renderables();

        let ctx = RenderContext {
            gpu: &self.gpu,
            command_encoder: self.gpu.create_command_encoder(Some("Rendering")),
            render_texture_views,
            logging: &self.logging,
            render_textures: &self.render_textures,
            transforms: &self.transforms,
            meshes: &self.meshes,
            materials: &self.materials,
            dynamic_materials: &self.dynamic_materials,
            pipelines: &self.pipelines,
            instances: &self.instances,
            bind_groups: &self.bind_groups,
            render_passes: &self.render_passes,
            anti_aliasing: &self.anti_aliasing,
            post_processing: &self.post_processing,
            clear_color: &self._clear_color,
            scene_spatial: &self.scene_spatial,
            material_classify_buffers: &self.material_classify_buffers,
            light_culling_buffers: &self.light_culling_buffers,
            live_punctual_count: self.lights.iter_active_punctual().count() as u32,
            live_light_count: self.lights.len() as u32,
            material_edge_buffers: self.material_edge_buffers.as_ref(),
            material_edge_layout_uniform: self.material_edge_layout_uniform.as_ref(),
            bind_group_layouts: &self.bind_group_layouts,
            camera: &self.camera,
            environment: &self.environment,
            shadows: &self.shadows,
            features: &self.features,
            compaction_buffers: self.compaction_buffers.as_ref(),
            coverage_buffers: self.coverage_buffers.as_ref(),
            // Filled in below once `collect_renderables` + the opaque
            // snapshot have produced the stats. Interior mutability
            // (Cell) so the per-pass code reads the final value through
            // an immutable `&RenderContext` without re-creating ctx.
            frame_optimizations: std::cell::Cell::new(
                crate::optimization_policy::FrameOptimizations::default(),
            ),
            viewport_size,
            prep_config: &self.prep_config,
        };

        // Snapshot per-opaque-renderable info that the occlusion + indirect-
        // draw infrastructure needs after `renderables.opaque` is consumed
        // by the material-opaque pass. For each opaque mesh-renderable
        // with a world AABB:
        //   - `aabb`               → cull pass instance bounds
        //   - `mesh_meta_offset`   → compaction shader's slot identifier
        //                            (`mesh_meta_offset / 256 = slot`)
        //   - `index_count`        → drawIndirect args (static field
        //                            populated by CPU; instance_count is
        //                            GPU-populated by the compaction shader)
        //   - `instanced`          → instanced meshes stay on the legacy
        //                            `draw_indexed_with_instance_count`
        //                            path and don't get a `drawIndirect`
        //                            args entry
        struct OcclusionSnapshot {
            aabb: crate::bounds::Aabb,
            mesh_meta_offset: u32,
            index_count: u32,
        }
        // Instanced meshes stay on the legacy
        // `draw_indexed_with_instance_count` path (their `instance_index`
        // ranges would collide across meshes in the shared storage-array
        // meta lookup), so they don't need cull-pass instances or
        // IndirectDrawArgs slots — skip them here.
        let opaque_snapshots: Vec<OcclusionSnapshot> = renderables
            .opaque
            .iter()
            .filter_map(|r| {
                if r.instanced {
                    return None;
                }
                let aabb = r.world_aabb.clone()?;
                let meta_offset = ctx.meshes.meta.geometry_buffer_offset(r.key).ok()? as u32;
                let buffer_info = ctx.meshes.buffer_info(r.key).ok()?;
                let index_count = buffer_info.triangles.vertex_attribute_indices.count as u32;
                Some(OcclusionSnapshot {
                    aabb,
                    mesh_meta_offset: meta_offset,
                    index_count,
                })
            })
            .collect();

        // Compute this frame's optimization decision now that we have
        // the renderable counts + opaque snapshot. The pure function
        // takes (policy, stats, prev_frame, frames_in_current_mode)
        // and emits the new `FrameOptimizations`. Tests live in
        // `crate::optimization_policy::tests`.
        let frame_opts_stats = crate::optimization_policy::FrameOptimizationStats {
            features_gpu_culling: self.features.gpu_culling,
            features_decals: self.features.decals,
            opaque_count: renderables.opaque.len() as u32,
            non_instanced_with_aabb_count: opaque_snapshots.len() as u32,
            decals_count: self.decals.as_ref().map(|d| d.len() as u32).unwrap_or(0),
            args_ready: self
                .compaction_buffers
                .as_ref()
                .map(|cb| cb.args_ready.get())
                .unwrap_or(false),
        };
        let frame_opts = crate::optimization_policy::compute_frame_optimizations(
            &self.optimization_policy,
            &frame_opts_stats,
            &self.frame_optimizations,
            self.frames_in_current_mode,
        );
        ctx.frame_optimizations.set(frame_opts);
        // Cooldown bookkeeping uses `gpu_occlusion` as the flip
        // detector; the other derived flags follow from inputs the
        // policy doesn't gate on. Computed here while we hold the
        // pre-update prev-frame state, then applied at end of
        // `render()` (after `renderables` is dropped) so we can take
        // `&mut self` to write the renderer state.
        let next_frames_in_current_mode = if frame_opts.stable_mode(&self.frame_optimizations) {
            self.frames_in_current_mode.saturating_add(1)
        } else {
            1
        };

        // Args-buffer poisoning rule: when `gpu_occlusion` is false
        // (Off, Auto-disengaged, or capability missing) the compaction
        // pass won't run, so any `args_ready=true` from a prior frame
        // would let `mesh.rs` issue stale `drawIndirect` calls. Clear
        // the flag so a future re-enable warms up through one frame of
        // CPU geometry before drawIndirect resumes.
        if !frame_opts.gpu_occlusion {
            if let Some(compaction_buffers) = self.compaction_buffers.as_ref() {
                compaction_buffers.args_ready.set(false);
            }
        }

        if let Some(hook) = hooks.and_then(|h| h.first_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                    Some(tracing::span!(tracing::Level::INFO, "FirstPass Hook").entered())
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Geometry RenderPass").entered())
            } else {
                None
            };

            self.render_passes
                .geometry
                .render(&ctx, renderables.opaque, false)?;
        }

        // Skip the HUD geometry pass entirely when nothing's drawn into it.
        // The pass body unconditionally opens a render pass on the same 4
        // MRT visibility targets (+ HUD depth) with `LoadOp::Load`, which
        // on a TBR mobile GPU is the worst-case antipattern: full-screen
        // tile-store of the just-written world MRTs back to off-chip RAM,
        // then immediate tile-load back in, for zero drawn pixels. At a
        // 400×800 mobile viewport the wasted bandwidth is ~40 MB per
        // frame, every frame, when HUD is empty. The same skip applies to
        // the HUD transparent + HUD line passes further below.
        if !renderables.hud.is_empty() {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "HUD Geometry RenderPass").entered())
            } else {
                None
            };

            self.render_passes
                .geometry
                .render(&ctx, renderables.hud, true)?;
        }

        if let Some(hook) = hooks.and_then(|h| h.after_geometry_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                    Some(tracing::span!(tracing::Level::INFO, "AfterGeometryPass Hook").entered())
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        // GPU coverage tally — one atomicAdd per pixel into the
        // per-mesh counts buffer. Runs after the geometry passes (so
        // the full visibility buffer is populated). The
        // `copyBufferToBuffer` that primes the readback buffer is
        // recorded *only* when the prior frame's `mapAsync` has
        // resolved (single-buffered readback path — writing to a
        // pending-map buffer is a WebGPU validation error). When the
        // prior readback is still in flight we just drop this frame's
        // coverage signal; downstream consumers fall back to
        // "conservatively visible". Skipped entirely when
        // `features.coverage_lod == false`.
        let kick_coverage_readback = if let (Some(coverage_pass), Some(coverage_buffers)) = (
            self.render_passes.coverage.as_ref(),
            self.coverage_buffers.as_ref(),
        ) {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Coverage RenderPass").entered())
            } else {
                None
            };
            // Zero the per-mesh atomic counts via a recorded
            // `clear_buffer` so it runs in command order strictly before
            // the coverage compute dispatch reads + atomic-adds into
            // them. Previously this was a `queue.writeBuffer` of
            // `capacity * 4` bytes of zeros, every byte shipped across
            // the wasm↔JS boundary — moving to `clear_buffer` zeroes
            // the GPU buffer in-place with no host upload.
            coverage_buffers.reset_counts(&ctx.command_encoder);
            coverage_pass.render(&ctx)?;
            let prior_inflight = self.coverage_readback_state.lock().unwrap().inflight;
            if !prior_inflight {
                let bytes_to_copy = coverage_buffers.capacity.saturating_mul(4);
                ctx.command_encoder.copy_buffer_to_buffer(
                    &coverage_buffers.counts_buffer,
                    0,
                    &coverage_buffers.readback_buffer,
                    0,
                    bytes_to_copy,
                )?;
                true
            } else {
                false
            }
        } else {
            false
        };

        // Shadow generation pass — runs between the geometry passes
        // and light culling so the shading passes downstream sample
        // the freshly-written shadow maps. Short-circuits when there
        // are no active shadow casters.
        if self.shadows.any_active() {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Shadow Generation").entered())
            } else {
                None
            };
            crate::shadows::render_pass::record(&ctx, &self.shadows)?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Light Culling RenderPass").entered())
            } else {
                None
            };

            self.render_passes.light_culling.render(&ctx)?;
        }

        // Kick the GPU light-culling overflow readback copy. Same
        // discipline as the edge-overflow readback below:
        // `copy_buffer_to_buffer` is recorded INTO the command encoder
        // so it executes strictly after the cull dispatch; the
        // `mapAsync` spawn-local lives below `submit_commands` so the
        // GPU sees the copy before the host requests the map.
        // Single-buffered via the `inflight` gate.
        let kick_froxel_overflow_readback = {
            let inflight = self.froxel_overflow_readback_state.lock().unwrap().inflight;
            if !inflight && ctx.live_punctual_count > 0 {
                ctx.command_encoder.copy_buffer_to_buffer(
                    &self.light_culling_buffers.overflow_buffer,
                    0,
                    &self.light_culling_buffers.overflow_readback_buffer,
                    0,
                    crate::render_passes::light_culling::buffers::OVERFLOW_READBACK_BYTES as u32,
                )?;
                true
            } else {
                false
            }
        };

        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Clear opaque").entered())
            } else {
                None
            };

            self.render_textures.clear_opaque(&self.gpu)?;
        }

        // Material classify: per-tile scan of the visibility buffer
        // produces the indirect-dispatch args + tile buckets the
        // opaque pipelines consume below. Runs once per frame; cheap
        // (~few hundred microseconds on a 4K viewport).
        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Material Classify RenderPass").entered())
            } else {
                None
            };
            // Priority 3 — reset the edge-buffer header (counters +
            // indirect-args) before classify rebuilds them this frame.
            // Cheap: ~64 bytes per write at typical bucket counts.
            if let Some(edge_buffers) = ctx.material_edge_buffers {
                edge_buffers.reset_header(&self.gpu)?;
            }

            self.render_passes.material_classify.render(&ctx)?;
        }

        // Material prep (Plan B) — shared, material-independent per-pixel
        // resolve. `Some` only when `PrepPassConfig.enabled`, so this is the
        // gate: with the flag off the pass is `None` and the legacy path is
        // byte-identical. Dispatched between classify and opaque; its outputs
        // are inert (unread) until later Plan B stages consume them.
        if let Some(prep) = self.render_passes.material_prep.as_ref() {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Material Prep RenderPass").entered())
            } else {
                None
            };
            prep.render(&ctx)?;
            // Stage 5b-shadow: after the full-screen cs_prep, fill the compact
            // per-edge-sample shadow texture (MSAA only — no-op otherwise) so the
            // MSAA `cs_edge` reads it instead of inline-sampling shadow maps.
            // classify already populated edge_to_xy + edge_count this frame
            // (classify → prep → opaque ordering); concurrent edge-buffer READ is
            // safe within the frame encoder.
            prep.render_edge(&ctx)?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Material Opaque RenderPass").entered())
            } else {
                None
            };

            self.render_passes
                .material_opaque
                .render(&ctx, renderables.opaque)?;

            // Per-shader-id MSAA edge-resolve + final blend (Priority
            // 3 in https://github.com/dakom/awsm-renderer/pull/99). No-op when MSAA
            // is off or the edge_resolve pipelines haven't been
            // submitted-and-resolved yet (warn-and-skip per
            // pipeline_scheduler::warn_pipeline_not_compiled).
            self.render_passes
                .material_opaque
                .render_edge_resolve(&ctx)?;
        }

        // Kick the edge-overflow readback copy. `copy_buffer_to_buffer`
        // is recorded INTO the command encoder so it executes strictly
        // after the classify dispatch (and after the edge_resolve /
        // final_blend dispatches that also touched edge_data); the
        // `mapAsync` spawn-local lives below the `submit_commands`
        // call so the GPU sees the copy before the host requests the
        // map. Single-buffered (`inflight` gate) per the comment on
        // `EdgeOverflowReadbackState`.
        let kick_edge_overflow_readback = if let Some(edge_buffers) =
            self.material_edge_buffers.as_ref()
        {
            let inflight = self.edge_overflow_readback_state.lock().unwrap().inflight;
            if !inflight {
                ctx.command_encoder.copy_buffer_to_buffer(
                    &edge_buffers.data_buffer,
                    0,
                    &edge_buffers.overflow_readback_buffer,
                    0,
                    crate::render_passes::material_opaque::edge_buffers::EDGE_OVERFLOW_READBACK_BYTES,
                )?;
                true
            } else {
                false
            }
        } else {
            false
        };

        // Build the opaque RT mip chain when any visible transparent
        // material uses transmission. The transparent pass uses these
        // mips for hardware-filtered background sampling at refraction
        // points instead of a multi-tap blur. Skipped entirely on frames
        // with no transmissive material — they pay zero overhead.
        let scene_has_transmission = renderables
            .transparent
            .iter()
            .any(|r| self.materials.has_transmission(r.material_key()));
        if scene_has_transmission {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Opaque Mipgen").entered())
            } else {
                None
            };
            // Clone the texture handle and mip count out of the inner
            // borrow first; that drops the immutable `self.render_textures`
            // borrow before we take a mutable borrow on `self.opaque_mipgen`.
            // GpuTexture is a wasm-bindgen JS handle — `.clone()` is a
            // refcount bump, not a texture copy.
            let opaque_info = self
                .render_textures
                .inner()
                .map(|inner| (inner.opaque.clone(), inner.opaque_mip_count));
            // The mipgen caches per-mip views + bind groups across
            // frames. We invalidate explicitly any time
            // `RenderTexturesInner` was rebuilt this frame (viewport
            // resize, AA flip, T2.5 mip-chain grow, T2.6 HUD-depth
            // grow) so the cache stays paired with the right
            // `GpuTexture` identity.
            if ctx.render_texture_views.views_recreated {
                self.opaque_mipgen.invalidate();
            }
            if let Some((texture, mip_count)) = opaque_info {
                self.opaque_mipgen
                    .record(&self.gpu, &ctx.command_encoder, &texture, mip_count)?;
            }
        }

        {
            let _maybe_span_guard = if ctx.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Opaque to Transparent Blit").entered())
            } else {
                None
            };

            blit_tex(
                match &ctx.anti_aliasing.msaa_sample_count {
                    Some(sample_count) if *sample_count == 4 => {
                        &ctx.render_textures
                            .opaque_to_transparent_blit_pipeline_msaa_4
                    }
                    None => {
                        &ctx.render_textures
                            .opaque_to_transparent_blit_pipeline_no_anti_alias
                    }
                    Some(count) => {
                        return Err(AwsmError::UnsupportedMsaaCount(*count));
                    }
                },
                match &ctx.anti_aliasing.msaa_sample_count {
                    Some(sample_count) if *sample_count == 4 => {
                        &ctx.render_texture_views
                            .opaque_to_transparent_blit_bind_group_msaa_4
                    }
                    None => {
                        &ctx.render_texture_views
                            .opaque_to_transparent_blit_bind_group_no_anti_alias
                    }
                    Some(count) => {
                        return Err(AwsmError::UnsupportedMsaaCount(*count));
                    }
                },
                &ctx.render_texture_views.transparent,
                &ctx.command_encoder,
            )?;
        }

        // HZB build. Runs after opaque shading so the depth buffer
        // holds the final scene depth, but BEFORE the decal pass — the
        // decal classify uses the HZB to gate per-tile decal append.
        // Also consumed by the occlusion-cull pass below.
        //
        // Allocation gate is `features.gpu_culling` (the HZB texture
        // lives behind that Option). Per-frame engagement is
        // `frame_optimizations.hzb`, which the policy derives as
        // `gpu_occlusion || decal_hzb_gate` — so we still rebuild HZB
        // when decals are active even if mesh occlusion is disengaged
        // this frame.
        if frame_opts.hzb {
            if let Some(hzb) = self.render_passes.hzb.as_ref() {
                let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                    Some(tracing::span!(tracing::Level::INFO, "HZB RenderPass").entered())
                } else {
                    None
                };
                hzb.render(&ctx)?;
            }
        }

        // Projection decals. Runs after the blit so `transparent_tex`
        // already holds the opaque shading result; the decal pass
        // overwrites the small subset of pixels its volumes cover with
        // the alpha-blended composite, leaving every other pixel as
        // the blit produced it. No-op when no decals are active or
        // MSAA is on (the v1 path doesn't have a multisampled
        // storage-binding target — see
        // `MaterialDecalRenderPass::render`). Skipped entirely when
        // `features.decals == false`.
        //
        // Order constraint: must run AFTER the HZB build above so
        // the per-tile classify reads a populated HZB. The previous
        // arrangement built HZB after the decal pass, leaving the
        // classify reading an empty/stale HZB on every frame.
        if let (Some(material_decal), Some(decals)) = (
            self.render_passes.material_decal.as_ref(),
            self.decals.as_ref(),
        ) {
            let _maybe_span_guard = if ctx.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Material Decal RenderPass").entered())
            } else {
                None
            };
            // Zero the per-tile atomic counts before classify reads
            // them. Recorded into the command encoder so it runs in
            // command order strictly before the classify dispatch
            // (queue.writeBuffer wouldn't — see the matching comment
            // on the args-buffer clear in the occlusion block).
            if let Some(decal_classify_buffers) = self.decal_classify_buffers.as_ref() {
                decal_classify_buffers.reset_counts(&ctx.command_encoder);
            }
            material_decal.render(&ctx, decals)?;
        }

        // Occlusion cull. Pack the active opaque renderables' world
        // AABBs + mesh_meta_offset into the GPU instance buffer,
        // then dispatch a compute shader that frustum + HZB tests
        // each. The compaction step below atomicAdds 1 per visible
        // instance into the matching mesh's
        // `IndirectDrawArgs.instance_count`; the geometry pass under
        // `features.gpu_culling` consumes that via `drawIndirect`.
        // Skipped entirely when `features.gpu_culling == false`
        // (see `RendererFeatures` in features.rs).
        // Gate the entire cull + compaction block on the per-frame
        // policy decision. `features.gpu_culling` allocates the
        // resources; `frame_opts.gpu_occlusion` engages the work.
        // When this is `false`, the args-buffer poison above already
        // flipped `args_ready` so the geometry pass routes through the
        // CPU branch — and the cull/compaction passes are simply
        // skipped, saving the compute dispatches on small scenes.
        if frame_opts.gpu_occlusion {
            if let (Some(occlusion_buffers), Some(occlusion_pass)) = (
                self.occlusion_buffers.as_ref(),
                self.render_passes.occlusion.as_ref(),
            ) {
                // Pack the OcclusionInstance buffer. The compaction shader
                // now owns the full `IndirectDrawArgs` slot write — see
                // compaction.wgsl for why the previous CPU
                // `queue.writeBuffer` of args was a race against the
                // already-recorded geometry pass. Instanced meshes are
                // still uploaded (they need to be cull-tested for their
                // own bookkeeping if instancing extensions reuse this
                // buffer) but compaction skips slot writes for slots used
                // by instanced meshes via `mesh_meta_offset` lookup; in
                // the current single-instance path each non-instanced
                // mesh owns its own slot, so there's no contention.
                let occlusion_instance_count = {
                    let stride =
                        crate::render_passes::occlusion::buffers::OCCLUSION_INSTANCE_STRIDE;
                    let mut bytes: Vec<u8> = Vec::with_capacity(opaque_snapshots.len() * stride);
                    for snap in &opaque_snapshots {
                        bytes.extend_from_slice(&snap.aabb.min.x.to_le_bytes());
                        bytes.extend_from_slice(&snap.aabb.min.y.to_le_bytes());
                        bytes.extend_from_slice(&snap.aabb.min.z.to_le_bytes());
                        bytes.extend_from_slice(&0u32.to_le_bytes()); // _pad0
                        bytes.extend_from_slice(&snap.aabb.max.x.to_le_bytes());
                        bytes.extend_from_slice(&snap.aabb.max.y.to_le_bytes());
                        bytes.extend_from_slice(&snap.aabb.max.z.to_le_bytes());
                        bytes.extend_from_slice(&0u32.to_le_bytes()); // _pad1
                                                                      // mesh_meta_offset — the compaction shader divides
                                                                      // by `MaterialMeshMeta` stride (256 B) to derive
                                                                      // the args-buffer slot; the geometry meta uses
                                                                      // the same alignment so this byte offset works.
                        bytes.extend_from_slice(&snap.mesh_meta_offset.to_le_bytes());
                        bytes.extend_from_slice(&0u32.to_le_bytes()); // instance_attr_base
                                                                      // index_count flows through cull → compaction so
                                                                      // the compaction shader can write the static
                                                                      // `IndirectDrawArgs.index_count` field itself.
                                                                      // (Compaction also synthesises `first_instance =
                                                                      // mesh_slot`; first_index / base_vertex stay at
                                                                      // zero from the `clear_buffer` below.)
                        bytes.extend_from_slice(&snap.index_count.to_le_bytes());
                        bytes.extend_from_slice(&0u32.to_le_bytes()); // _pad2
                    }
                    let count = (bytes.len() / stride) as u32;
                    if count > 0 {
                        occlusion_buffers.write_instances(&self.gpu, bytes.as_slice())?;
                    }
                    count
                };
                // Write the `OcclusionParams` uniform every frame so the
                // cull + compaction shaders bound their per-thread loops
                // by this frame's active count rather than the buffer's
                // capacity. Without this, tail threads in the
                // workgroup-rounded dispatch process stale instance slots
                // and double-count phantom meshes into
                // `IndirectDrawArgs.instance_count`.
                occlusion_buffers.write_params(&self.gpu, occlusion_instance_count)?;

                // Clear the IndirectDrawArgs buffer in COMMAND order so it
                // executes strictly after the geometry pass's earlier
                // `draw_indexed_indirect` reads of this same buffer and
                // strictly before the compaction shader's writes. A CPU-
                // side `queue.writeBuffer` would have raced ahead of the
                // recorded geometry pass (queue order ≠ command order),
                // zeroing the args that an in-flight drawIndirect needed
                // to read. Compaction repopulates the static fields
                // (`index_count`, `first_instance`) and atomicAdds
                // `instance_count` from this zero base; slots not touched
                // by compaction stay zero, which makes their drawIndirect
                // a 0-index, 0-instance no-op (correct for meshes that
                // dropped out of the renderable set this frame).
                if let Some(compaction_buffers) = self.compaction_buffers.as_ref() {
                    ctx.command_encoder
                        .clear_buffer(&compaction_buffers.args_buffer, None, None);
                }

                if occlusion_instance_count > 0 {
                    let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                        Some(
                            tracing::span!(
                                tracing::Level::INFO,
                                "Occlusion Cull RenderPass",
                                instances = occlusion_instance_count
                            )
                            .entered(),
                        )
                    } else {
                        None
                    };
                    occlusion_pass.render(&ctx, occlusion_instance_count)?;

                    // Compact the cull's `visible_this_frame[]` into
                    // per-mesh `IndirectDrawArgs.instance_count` — the
                    // *next* frame's geometry pass `drawIndirect`
                    // consumer reads this. (The geometry pass at the
                    // top of this frame already consumed last frame's
                    // compaction result; that's the one-frame-latent
                    // visibility model documented on
                    // `CompactionBuffers::args_ready`.)
                    if let Some(compaction_pass) = self.render_passes.occlusion_compaction.as_ref()
                    {
                        let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                            Some(
                                tracing::span!(
                                    tracing::Level::INFO,
                                    "Occlusion Compaction",
                                    instances = occlusion_instance_count
                                )
                                .entered(),
                            )
                        } else {
                            None
                        };
                        compaction_pass.render(&ctx, occlusion_instance_count)?;
                        // Mark the args buffer as containing a valid
                        // previous-frame visibility set for the next
                        // frame's geometry pass. The `ensure_capacity`
                        // resize earlier in this function constructs a
                        // fresh zero-initialized `CompactionBuffers`
                        // which resets this back to `false`, so a
                        // grow event correctly flips us back to the
                        // CPU path until the next compaction completes.
                        if let Some(compaction_buffers) = self.compaction_buffers.as_ref() {
                            compaction_buffers.args_ready.set(true);
                        }
                    }
                } else {
                    // Zero-instance frame: the `clear_buffer` above
                    // zeroed args, but no compaction ran to repopulate
                    // them. If `args_ready` carried `true` from a prior
                    // frame, the next frame's geometry pass would
                    // drawIndirect against the cleared (all-zero) buffer
                    // — every non-instanced AABB mesh would vanish for
                    // one frame on the way back in. Poison it now so the
                    // next frame routes through the CPU path until a
                    // real compaction lands.
                    if let Some(compaction_buffers) = self.compaction_buffers.as_ref() {
                        compaction_buffers.args_ready.set(false);
                    }
                }
            }
        }

        // Built-in line render pass — must run after the opaque->transparent
        // blit (so depth + transparent target are populated) and before any
        // `before_transparent_pass` hook so editor overlays can draw on top.
        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Line RenderPass").entered())
            } else {
                None
            };
            self.lines.render(&ctx)?;
        }

        if let Some(hook) = hooks.and_then(|h| h.before_transparent_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                    Some(
                        tracing::span!(tracing::Level::INFO, "BeforeTransparentPass Hook")
                            .entered(),
                    )
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(
                    tracing::span!(tracing::Level::INFO, "Material Transparent RenderPass")
                        .entered(),
                )
            } else {
                None
            };

            self.render_passes
                .material_transparent
                .render(&ctx, renderables.transparent, false)?;
        }

        if let Some(hook) = hooks.and_then(|h| h.after_transparent_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                    Some(
                        tracing::span!(tracing::Level::INFO, "AfterTransparentPass Hook").entered(),
                    )
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        // HUD transparent / lit pass. Same skip-on-empty rationale as the
        // HUD geometry pass above — both attachments are LoadOp::Load on
        // the world-state textures + HUD depth, so emitting the pass
        // descriptor with zero draws still costs a full-screen tile
        // round-trip on TBR mobile GPUs.
        if !renderables.hud.is_empty() {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "HUD RenderPass").entered())
            } else {
                None
            };

            self.render_passes
                .material_transparent
                .render(&ctx, renderables.hud, true)?;
        }

        // if None, it's handled by MSAA resolve in transparent pass
        if let Some(bind_group) = &ctx
            .render_texture_views
            .transparent_to_composite_blit_bind_group_no_anti_alias
        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(
                    tracing::span!(tracing::Level::INFO, "Non-antialised composite blit").entered(),
                )
            } else {
                None
            };

            blit_tex(
                &ctx.render_textures
                    .transparent_to_composite_blit_pipeline_no_anti_alias,
                bind_group,
                &ctx.render_texture_views.composite,
                &ctx.command_encoder,
            )?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Effects RenderPass").entered())
            } else {
                None
            };

            self.render_passes.effects.render(&ctx)?;
        }

        {
            let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Display RenderPass").entered())
            } else {
                None
            };

            self.render_passes.display.render(&ctx)?;
        }

        if let Some(hook) = hooks.and_then(|h| h.last_pass.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                    Some(tracing::span!(tracing::Level::INFO, "LastPass Hook").entered())
                } else {
                    None
                };
                hook(&ctx)?;
            }
        }

        // Build the slot → MeshKey map now (while we still own
        // `self.meshes`) so the async readback task can route raw
        // `u32` slot counts back into the `MeshCoverage` table
        // without re-borrowing the renderer. Only collected when
        // this frame actually recorded a copy-to-readback — the
        // encoder block above gates the copy on
        // `coverage_readback_state.inflight`, so a missed copy
        // means there's nothing for the spawn_local task to read.
        //
        // Slot indexing uses the *material* meta offset (not the
        // geometry meta) because the visibility-buffer's per-pixel
        // `material_mesh_meta_offset` is what the geometry fragment
        // wrote and what the coverage compute shader divides by
        // `MATERIAL_MESH_META_BYTE_ALIGNMENT` to index the counts
        // buffer. The geometry and material meta SecondaryMaps can
        // assign different slot indices to the same `MeshKey` under
        // fragmentation, so reading `geometry_buffer_offset` here
        // would silently miss.
        let coverage_slot_map: Option<Vec<(crate::meshes::MeshKey, usize)>> =
            if kick_coverage_readback {
                Some(
                    self.meshes
                        .iter()
                        .filter_map(|(mesh_key, _)| {
                            let off = self.meshes.meta.material_buffer_offset(mesh_key).ok()?;
                            let slot = off
                                / crate::meshes::meta::material_meta::MATERIAL_MESH_META_BYTE_ALIGNMENT;
                            Some((mesh_key, slot))
                        })
                        .collect(),
                )
            } else {
                None
            };

        self.gpu.submit_commands(&ctx.command_encoder.finish());

        // Kick the `mapAsync` readback so next frame's
        // `MeshCoverage::ingest` sees this frame's counts. Skipped
        // when a previous map hasn't yet resolved (single-buffered
        // path — under high mapping latency we lose a frame of
        // coverage rather than ringing the buffer). The
        // `coverage_buffers` here is always `Some` because
        // `kick_coverage_readback` was only set to `true` inside the
        // earlier `if let Some(coverage_buffers)` arm.
        if let (Some(slot_map), Some(coverage_buffers)) =
            (coverage_slot_map, self.coverage_buffers.as_ref())
        {
            let readback_buffer = coverage_buffers.readback_buffer.clone();
            let readback_size_bytes = coverage_buffers.capacity.saturating_mul(4);
            let state = std::sync::Arc::clone(&self.coverage_readback_state);
            state.lock().unwrap().inflight = true;
            wasm_bindgen_futures::spawn_local(async move {
                let result = crate::core::buffers::extract_buffer_vec(
                    &readback_buffer,
                    Some(readback_size_bytes),
                )
                .await;
                let snapshot: Vec<(crate::meshes::MeshKey, u32)> = match result {
                    Ok(bytes) => slot_map
                        .into_iter()
                        .filter_map(|(mesh_key, slot)| {
                            let base = slot * 4;
                            if base + 4 > bytes.len() {
                                return None;
                            }
                            let count = u32::from_le_bytes(bytes[base..base + 4].try_into().ok()?);
                            Some((mesh_key, count))
                        })
                        .collect(),
                    Err(err) => {
                        tracing::warn!("coverage readback mapAsync failed: {err:?}");
                        Vec::new()
                    }
                };
                let mut state = state.lock().unwrap();
                state.pending_snapshot = Some(snapshot);
                state.inflight = false;
            });
        }

        // Kick the MSAA edge-overflow `mapAsync` readback. Same shape
        // as the coverage readback above: the copy was recorded into
        // the command encoder before submit; this just hands control
        // to the host to map the resolved buffer when the GPU finishes.
        // Resolves a frame or two later; the next render preamble
        // ingests `pending_overflow_count` and calls
        // `set_max_edge_budget(current * 2)` if overflow > 0. The
        // `material_edge_buffers` here is always `Some` because
        // `kick_edge_overflow_readback` was only set true inside the
        // matching `if let Some(edge_buffers)` arm.
        if kick_edge_overflow_readback {
            if let Some(edge_buffers) = self.material_edge_buffers.as_ref() {
                let readback_buffer = edge_buffers.overflow_readback_buffer.clone();
                let state = std::sync::Arc::clone(&self.edge_overflow_readback_state);
                state.lock().unwrap().inflight = true;
                wasm_bindgen_futures::spawn_local(async move {
                    let result = crate::core::buffers::extract_buffer_vec(
                        &readback_buffer,
                        Some(crate::render_passes::material_opaque::edge_buffers::EDGE_OVERFLOW_READBACK_BYTES),
                    )
                    .await;
                    let snapshot: Option<(u32, u32)> = match result {
                        Ok(bytes) if bytes.len() >= 8 => {
                            let edge_count = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
                            let overflow_count =
                                u32::from_le_bytes(bytes[4..8].try_into().unwrap());
                            Some((edge_count, overflow_count))
                        }
                        Ok(_) => None,
                        Err(err) => {
                            tracing::warn!(
                                target: "awsm_renderer::edge_resolve",
                                "edge-overflow readback mapAsync failed: {err:?}",
                            );
                            None
                        }
                    };
                    let mut state = state.lock().unwrap();
                    state.pending_overflow_count = snapshot;
                    state.inflight = false;
                });
            }
        }

        // Kick the GPU light-culling `mapAsync` readback. Same shape
        // as the edge-overflow readback above.
        if kick_froxel_overflow_readback {
            let readback_buffer = self.light_culling_buffers.overflow_readback_buffer.clone();
            let state = std::sync::Arc::clone(&self.froxel_overflow_readback_state);
            state.lock().unwrap().inflight = true;
            wasm_bindgen_futures::spawn_local(async move {
                let result = crate::core::buffers::extract_buffer_vec(
                    &readback_buffer,
                    Some(
                        crate::render_passes::light_culling::buffers::OVERFLOW_READBACK_BYTES
                            as u32,
                    ),
                )
                .await;
                let snapshot: Option<u32> = match result {
                    Ok(bytes) if bytes.len() >= 4 => {
                        Some(u32::from_le_bytes(bytes[0..4].try_into().unwrap()))
                    }
                    Ok(_) => None,
                    Err(err) => {
                        tracing::warn!(
                            target: "awsm_renderer::light_culling",
                            "froxel-overflow readback mapAsync failed: {err:?}",
                        );
                        None
                    }
                };
                let mut state = state.lock().unwrap();
                state.pending_overflow_count = snapshot;
                state.inflight = false;
            });
        }

        if let Some(hook) = hooks.and_then(|h| h.post_render.as_ref()) {
            {
                let _maybe_span_guard = if self.logging.render_timings.sub_frame() {
                    Some(tracing::span!(tracing::Level::INFO, "PostRender Hook").entered())
                } else {
                    None
                };
                hook(self)?;
            }
        }

        // Commit the deferred optimization-policy bookkeeping. We
        // couldn't write these earlier because `renderables` held a
        // borrow on `&self` through the rest of the render flow.
        self.frame_optimizations = frame_opts;
        self.frames_in_current_mode = next_frames_in_current_mode;

        Ok(())
    }

    /// Auto-drive step 1 (sync, non-blocking) for HUD transparent
    /// pipelines. When a HUD mesh exists and the resolve signature
    /// (texture-pool array/sampler counts × MSAA × HUD revision) has
    /// changed since the last completed resolve, (re)resolve every
    /// `mesh.hud` mesh's transparent pipeline variant:
    ///
    ///  1. Issue the per-mesh transparent shaders synchronously
    ///     (skip-validate — errors resurface as pipeline-creation
    ///     rejections), so the cache keys can be built without awaiting.
    ///  2. Build the per-mesh pipeline cache keys (now a sync cache hit
    ///     via `now_or_never`).
    ///  3. Install already-cached variants immediately; for genuine
    ///     misses, issue the `createRenderPipelineAsync` promises and
    ///     stash them for [`Self::poll_hud_resolve`] to install.
    ///
    /// Early-outs to a single bool check when no HUD mesh has ever been
    /// inserted (`has_seen_hud`), so non-HUD builds pay nothing. Skips
    /// while a previous resolve is still in flight (one batch at a time).
    fn kick_hud_resolve(&mut self) -> Result<()> {
        use crate::pipelines::render_pipeline::{RenderPipelineCacheKey, RenderPipelines};
        use crate::render_passes::material_transparent::pipeline::{
            MaterialTransparentPipelines, TransparentMeshPipelineRequest,
        };
        use crate::shaders::ShaderCacheKey;
        use futures::FutureExt;
        use std::collections::HashMap;

        if !self.meshes.has_seen_hud() || self.hud_resolve.inflight.is_some() {
            return Ok(());
        }

        let bind_groups = &self.render_passes.material_transparent.bind_groups;
        let sig = HudResolveSig {
            pool_arrays_len: bind_groups.texture_pool_arrays_len,
            pool_samplers_len: bind_groups.texture_pool_sampler_keys.len(),
            msaa: self.anti_aliasing.msaa_sample_count,
            hud_revision: self.meshes.hud_revision(),
        };
        if self.hud_resolve.last_sig == Some(sig) {
            return Ok(());
        }

        // Collect the HUD meshes. Only `mesh.hud` meshes are touched —
        // world transparents are resolved on the scene-load path, so we
        // neither duplicate that work nor risk re-resolving the world.
        let mut requests: Vec<TransparentMeshPipelineRequest> = Vec::new();
        for (mesh_key, mesh) in self.meshes.iter() {
            if !mesh.hud {
                continue;
            }
            // Only warm transparent pipelines for meshes that route to the
            // transparent pass — an opaque (incl. opaque-dynamic) material's
            // fragment can't compile against the transparent contract.
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
            requests.push(TransparentMeshPipelineRequest {
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
        if requests.is_empty() {
            self.hud_resolve.last_sig = Some(sig);
            return Ok(());
        }

        // Step 1: issue the shaders synchronously (skip validate).
        let shader_cache_keys = MaterialTransparentPipelines::shader_cache_keys_for_requests(
            &requests,
            bind_groups,
            &self.meshes.buffer_infos,
            &self.anti_aliasing,
        )?;
        self.shaders.ensure_keys_sync_skip_validate(
            &self.gpu,
            shader_cache_keys.into_iter().map(ShaderCacheKey::from),
        )?;

        // Step 2: build per-mesh pipeline cache keys. Every `get_key`
        // inside is a cache hit (shaders just issued above), so the
        // future resolves in a single `now_or_never` poll. If it
        // somehow doesn't (a shader cache miss slipped through), bail
        // without updating `last_sig` so the next frame retries — never
        // block or error on the render path.
        let cache_keys: Vec<RenderPipelineCacheKey> = match self
            .render_passes
            .material_transparent
            .pipelines
            .pipeline_cache_keys_for_requests(
                &self.gpu,
                &requests,
                &mut self.shaders,
                &self.render_passes.material_transparent.bind_groups,
                &self.meshes.buffer_infos,
                &self.anti_aliasing,
                &self.render_textures.formats,
            )
            .now_or_never()
        {
            Some(result) => result?,
            None => return Ok(()),
        };

        let mesh_keys: Vec<crate::meshes::MeshKey> = requests.iter().map(|r| r.mesh_key).collect();
        drop(requests);

        // Step 3: split cache hits (install now) from misses (compile).
        // Misses are deduped by cache key — many HUD meshes share one.
        let mut hit_mesh_keys: Vec<crate::meshes::MeshKey> = Vec::new();
        let mut hit_pipeline_keys: Vec<crate::pipelines::render_pipeline::RenderPipelineKey> =
            Vec::new();
        let mut unique_miss_keys: Vec<RenderPipelineCacheKey> = Vec::new();
        let mut miss_mesh_keys: Vec<Vec<crate::meshes::MeshKey>> = Vec::new();
        let mut idx_for_key: HashMap<RenderPipelineCacheKey, usize> = HashMap::new();
        for (mesh_key, cache_key) in mesh_keys.into_iter().zip(cache_keys) {
            if let Some(pipeline_key) = self.pipelines.render.get_cached_key(&cache_key) {
                hit_mesh_keys.push(mesh_key);
                hit_pipeline_keys.push(pipeline_key);
            } else if let Some(&u) = idx_for_key.get(&cache_key) {
                miss_mesh_keys[u].push(mesh_key);
            } else {
                let u = unique_miss_keys.len();
                idx_for_key.insert(cache_key.clone(), u);
                unique_miss_keys.push(cache_key);
                miss_mesh_keys.push(vec![mesh_key]);
            }
        }

        if !hit_mesh_keys.is_empty() {
            self.render_passes
                .material_transparent
                .pipelines
                .install_per_mesh_keys(hit_mesh_keys, hit_pipeline_keys);
        }

        if unique_miss_keys.is_empty() {
            // Everything was cache-hit — fully resolved this frame.
            self.hud_resolve.last_sig = Some(sig);
            return Ok(());
        }

        let mut prepped = RenderPipelines::ensure_keys_prepare(
            &self.gpu,
            &self.shaders,
            &self.pipeline_layouts,
            unique_miss_keys,
        )?;
        let promises = std::mem::take(&mut prepped.promises);
        let joined = Box::pin(futures::future::join_all(promises));
        self.hud_resolve.inflight = Some(HudResolveInflight {
            prep: prepped.prep,
            joined,
            miss_mesh_keys,
            sig,
        });
        Ok(())
    }

    /// Auto-drive step 2 (sync, non-blocking) for HUD transparent
    /// pipelines: poll the in-flight compile once with a no-op waker.
    /// Once every `createRenderPipelineAsync` promise resolves, install
    /// the pipelines into the cross-pass pool and fan each resolved key
    /// out to the HUD meshes that requested it. No-op when nothing is in
    /// flight.
    fn poll_hud_resolve(&mut self) -> Result<()> {
        use futures::FutureExt;

        let ready = match self.hud_resolve.inflight.as_mut() {
            Some(inflight) => inflight.joined.as_mut().now_or_never(),
            None => return Ok(()),
        };
        let Some(results) = ready else {
            return Ok(());
        };
        let inflight = self
            .hud_resolve
            .inflight
            .take()
            .expect("inflight present (just polled Some)");

        // Resolved keys come back in `unique_miss_keys` (== prep input)
        // order, which is parallel to `miss_mesh_keys`.
        let resolved = self
            .pipelines
            .render
            .ensure_keys_install(inflight.prep, results)?;
        for (mesh_keys, pipeline_key) in inflight.miss_mesh_keys.into_iter().zip(resolved) {
            let count = mesh_keys.len();
            self.render_passes
                .material_transparent
                .pipelines
                .install_per_mesh_keys(mesh_keys, std::iter::repeat_n(pipeline_key, count));
        }
        self.hud_resolve.last_sig = Some(inflight.sig);
        Ok(())
    }
}

/// Resolve-signature for the HUD transparent pipeline auto-drive. A
/// change in any axis means previously-resolved HUD pipeline variants
/// are stale: the texture-pool array/sampler counts and MSAA sample
/// count are baked into the transparent shader + pipeline cache keys,
/// and `hud_revision` bumps when a new HUD mesh appears.
#[derive(Clone, Copy, PartialEq, Eq)]
struct HudResolveSig {
    pool_arrays_len: u32,
    pool_samplers_len: usize,
    msaa: Option<u32>,
    hud_revision: u64,
}

/// In-flight HUD transparent pipeline compile, held between
/// [`AwsmRenderer::kick_hud_resolve`] issuing the
/// `createRenderPipelineAsync` promises and
/// [`AwsmRenderer::poll_hud_resolve`] installing them. The `joined`
/// future is `'static` (it only awaits the GPU promises), so the
/// borrow-free issue / poll / install split mirrors the line + material
/// schedulers.
struct HudResolveInflight {
    prep: crate::pipelines::render_pipeline::RenderPipelinesPrep,
    #[allow(clippy::type_complexity)]
    joined: std::pin::Pin<
        Box<
            dyn std::future::Future<
                Output = Vec<
                    std::result::Result<web_sys::GpuRenderPipeline, wasm_bindgen::JsValue>,
                >,
            >,
        >,
    >,
    /// Per-unique-miss (parallel to the prep inputs) list of HUD mesh
    /// keys whose pipeline key should be set to that resolved variant.
    miss_mesh_keys: Vec<Vec<crate::meshes::MeshKey>>,
    /// The signature this batch resolves; written to `last_sig` on
    /// install so a sig change *during* the compile re-triggers.
    sig: HudResolveSig,
}

/// Auto-drive state for HUD transparent pipeline resolution. `Default`
/// is the zero-cost idle state held by every renderer (including those
/// that never use HUD).
#[derive(Default)]
pub(crate) struct HudResolveState {
    last_sig: Option<HudResolveSig>,
    inflight: Option<HudResolveInflight>,
}

/// Context passed to render passes during a frame.
pub struct RenderContext<'a> {
    pub gpu: &'a AwsmRendererWebGpu,
    pub command_encoder: CommandEncoder,
    pub render_texture_views: RenderTextureViews,
    pub logging: &'a AwsmRendererLogging,
    pub render_textures: &'a RenderTextures,
    pub transforms: &'a Transforms,
    pub meshes: &'a Meshes,
    pub pipelines: &'a Pipelines,
    pub materials: &'a Materials,
    /// Runtime-registered dynamic-material registry. Read by the
    /// material_opaque dispatch loop to iterate the same bucket list
    /// (first-party + dynamic) the classify shader was compiled
    /// against. See [`crate::dynamic_materials`].
    pub dynamic_materials: &'a crate::dynamic_materials::DynamicMaterials,
    pub instances: &'a Instances,
    pub bind_groups: &'a BindGroups,
    pub render_passes: &'a RenderPasses,
    pub anti_aliasing: &'a AntiAliasing,
    pub post_processing: &'a PostProcessing,
    pub clear_color: &'a Color,
    /// Renderer-owned spatial index. Per-pass culling (camera + shadow)
    /// descends through this instead of walking `meshes` linearly.
    pub scene_spatial: &'a SceneSpatial,
    /// Classify-pass output. The opaque material pass uses this
    /// buffer both as a storage binding (for the per-bucket tile
    /// lookup) and as the indirect-args source for
    /// `dispatchWorkgroupsIndirect`.
    pub material_classify_buffers:
        &'a crate::render_passes::material_classify::buffers::ClassifyBuffers,
    /// GPU light-culling froxel buffers. The cull pass writes the
    /// per-froxel `(count, indices)`; the transparent + opaque-
    /// oversized shaders read it back at shading time.
    pub light_culling_buffers: &'a crate::render_passes::light_culling::LightCullingBuffers,
    /// Number of live punctual lights this frame — same value the
    /// cull and shading shaders see in `lights_info.data.x`. Used to
    /// gate the overflow readback (only punctuals can overflow a froxel).
    pub live_punctual_count: u32,
    /// Total live light count this frame (punctual + directional) —
    /// matches `lights_info.n_lights`. The froxel consumers walk the
    /// per-froxel slices whenever `n_lights > 0`, so the cull pass
    /// (sole writer/clearer of those counts) must run whenever this is
    /// non-zero; it may only skip when there are no lights at all.
    pub live_light_count: u32,
    /// Priority-3 MSAA edge-resolve composite buffer. `Some` only
    /// when MSAA is on (no edges to resolve under single-sample).
    /// Bound read-write by classify (slot 4 of group(0)) and by the
    /// per-shader edge_resolve / skybox_edge / final_blend pipelines.
    pub material_edge_buffers:
        Option<&'a crate::render_passes::material_opaque::edge_buffers::MaterialEdgeBuffers>,
    /// `EdgeBufferLayout` uniform companion. Same `Some` discipline.
    pub material_edge_layout_uniform: Option<&'a web_sys::GpuBuffer>,
    /// Bind-group-layout cache. Used by passes that build their own
    /// runtime bind groups inside `render()` (e.g. material-opaque's
    /// edge-resolve helpers).
    pub bind_group_layouts: &'a crate::bind_group_layout::BindGroupLayouts,
    /// Camera buffer — bound directly by the skybox edge-resolve
    /// pipeline. The primary opaque + main shading binds this through
    /// the opaque bind groups; the edge-resolve flow's standalone
    /// skybox shader needs it on its own group.
    pub camera: &'a crate::camera::CameraBuffer,
    /// Scene environment (skybox texture + sampler). Read by the
    /// skybox edge-resolve pipeline's standalone bind group.
    pub environment: &'a crate::environment::Environment,
    /// Shadow subsystem. Read by the edge-resolve flow to build the
    /// per-frame extended-shadows bind group (the 10 shadow resources
    /// plus the 2 edge-resolve resources, all bound at group(3) of the
    /// edge_resolve pipeline layout). Mirrors `BindGroupRecreateContext`
    /// for parity with the opaque-pass shadow bind-group construction.
    pub shadows: &'a crate::shadows::Shadows,
    /// Active feature gates. Read by the geometry pass to fork
    /// between `drawIndirect` (under `gpu_culling`) and the legacy
    /// CPU-recorded `draw_indexed_*` loop.
    pub features: &'a crate::features::RendererFeatures,
    /// GPU compaction `IndirectDrawArgs` buffer. `Some` only when
    /// `features.gpu_culling` is on. The geometry pass reads it as
    /// the indirect-args source for `drawIndirect`.
    pub compaction_buffers:
        Option<&'a crate::render_passes::occlusion::compaction::CompactionBuffers>,
    /// GPU mesh-pixel-coverage producer buffers. `Some` only when
    /// `features.coverage_lod` is on. The coverage render pass + the
    /// per-frame `copyBufferToBuffer` into the readback buffer both
    /// reach through this field.
    pub coverage_buffers: Option<&'a crate::render_passes::coverage::buffers::CoverageBuffers>,
    /// Per-frame derived flags from
    /// [`crate::optimization_policy::compute_frame_optimizations`].
    /// `Cell` because the values aren't known until after
    /// `collect_renderables` + the opaque snapshot have run, but
    /// `RenderContext` itself is constructed before that — pass-level
    /// code reads via `.get()`. Call sites consult this rather than
    /// `features` for runtime branching: `features.gpu_culling = true`
    /// means "the HZB/cull/compaction infrastructure exists,"
    /// `frame_optimizations.get().gpu_occlusion = true` means "run it
    /// this frame."
    pub frame_optimizations: std::cell::Cell<crate::optimization_policy::FrameOptimizations>,
    /// Cached `(width, height)` of the live swap-chain texture, snapped
    /// once at the top of `render()` and stable for the whole frame.
    /// Use this in place of repeated `gpu.current_context_texture_size()`
    /// calls — each call crosses the wasm↔JS boundary into
    /// `getCurrentTexture().getSize()`, which is small (~0.1–1 µs) but
    /// happens at multiple pass-level call sites per frame.
    pub viewport_size: (u32, u32),
    /// Plan B shared-prep config. Inert today — the prep pass is dispatched
    /// only when `render_passes.material_prep` is `Some` (i.e. when this is
    /// enabled). Threaded through so later stages can branch shading on it.
    pub prep_config: &'a crate::render_passes::material_prep::PrepPassConfig,
}

impl<'a> RenderContext<'a> {
    /// Live punctual-light count this frame. The cull pass + shading
    /// shaders both read this via `lights_info.data.x`; the cull pass's
    /// `render()` consults this CPU mirror to skip the dispatch when
    /// zero (typical for skybox-only / directional-only scenes).
    pub fn live_punctual_light_count(&self) -> u32 {
        self.live_punctual_count
    }

    /// Total live light count this frame (matches `lights_info.n_lights`).
    /// The cull pass uses this to decide whether it may skip entirely:
    /// the froxel consumers only walk per-froxel slices when
    /// `n_lights > 0`, so the cull (sole writer/clearer of those counts)
    /// must run whenever any light exists.
    pub fn live_light_count(&self) -> u32 {
        self.live_light_count
    }

    /// Begins a visibility-buffer extension pass for world-space opaque geometry.
    ///
    /// This pass loads the existing visibility attachments and world depth, allowing custom hooks
    /// to append opaque geometry before light culling/material-opaque shading.
    pub fn begin_world_geometry_extension_pass(
        &'a self,
        label: Option<&'a str>,
    ) -> Result<RenderPassEncoder> {
        self.command_encoder
            .begin_render_pass(
                &RenderPassDescriptor {
                    label,
                    color_attachments: vec![
                        ColorAttachment::new(
                            &self.render_texture_views.visibility_data,
                            LoadOp::Load,
                            StoreOp::Store,
                        ),
                        ColorAttachment::new(
                            &self.render_texture_views.barycentric,
                            LoadOp::Load,
                            StoreOp::Store,
                        ),
                        ColorAttachment::new(
                            &self.render_texture_views.normal_tangent,
                            LoadOp::Load,
                            StoreOp::Store,
                        ),
                        ColorAttachment::new(
                            &self.render_texture_views.barycentric_derivatives,
                            LoadOp::Load,
                            StoreOp::Store,
                        ),
                    ],
                    depth_stencil_attachment: Some(
                        DepthStencilAttachment::new(&self.render_texture_views.depth)
                            .with_depth_load_op(LoadOp::Load)
                            .with_depth_store_op(StoreOp::Store),
                    ),
                    ..Default::default()
                }
                .into(),
            )
            .map_err(Into::into)
    }

    /// Begins a world-space transparent effect pass that targets the transparent color buffer and
    /// shared scene depth.
    pub fn begin_world_transparent_pass(
        &'a self,
        label: Option<&'a str>,
    ) -> Result<RenderPassEncoder> {
        let mut color_attachment = ColorAttachment::new(
            &self.render_texture_views.transparent,
            LoadOp::Load,
            StoreOp::Store,
        );

        if self.anti_aliasing.msaa_sample_count.is_some() {
            color_attachment =
                color_attachment.with_resolve_target(&self.render_texture_views.composite);
        }

        self.command_encoder
            .begin_render_pass(
                &RenderPassDescriptor {
                    label,
                    color_attachments: vec![color_attachment],
                    depth_stencil_attachment: Some(
                        DepthStencilAttachment::new(&self.render_texture_views.depth)
                            .with_depth_load_op(LoadOp::Load)
                            .with_depth_store_op(StoreOp::Store),
                    ),
                    ..Default::default()
                }
                .into(),
            )
            .map_err(Into::into)
    }

    /// Begins a HUD transparent pass using the shared transparent color target and HUD depth.
    ///
    /// This matches the renderer's built-in HUD pass behavior:
    /// depth is cleared to `1.0` and then depth-tested/written within HUD space.
    pub fn begin_hud_transparent_pass(
        &'a self,
        label: Option<&'a str>,
    ) -> Result<RenderPassEncoder> {
        let mut color_attachment = ColorAttachment::new(
            &self.render_texture_views.transparent,
            LoadOp::Load,
            StoreOp::Store,
        );

        if self.anti_aliasing.msaa_sample_count.is_some() {
            color_attachment =
                color_attachment.with_resolve_target(&self.render_texture_views.composite);
        }

        // T2.6: `hud_depth` is Optional — built only after the first
        // HUD renderable has registered. This entry point is reachable
        // only from the HUD render-pass call sites, which are now
        // gated on `!renderables.hud.is_empty()` (T1.10) — by the
        // time we get here the HUD render group is non-empty, which
        // means at least one mesh has flipped `Meshes::has_seen_hud`
        // and the texture has been allocated. The expect is the
        // narrowed invariant.
        let hud_depth_view = self.render_texture_views.hud_depth.as_ref().expect(
            "hud_depth view absent at begin_hud_transparent_pass — invariant violated: \
             a HUD renderable must flip Meshes::has_seen_hud before any HUD pass call",
        );
        self.command_encoder
            .begin_render_pass(
                &RenderPassDescriptor {
                    label,
                    color_attachments: vec![color_attachment],
                    depth_stencil_attachment: Some(
                        DepthStencilAttachment::new(hud_depth_view)
                            .with_depth_load_op(LoadOp::Clear)
                            .with_depth_clear_value(1.0)
                            .with_depth_store_op(StoreOp::Store),
                    ),
                    ..Default::default()
                }
                .into(),
            )
            .map_err(Into::into)
    }

    /// Begins a pass that loads the already-rendered swapchain image.
    ///
    /// This is intended for `RenderHooks::last_pass` overlays, where you want to draw on top of
    /// the display output without clearing it.
    pub fn begin_display_overlay_pass(
        &'a self,
        label: Option<&'a str>,
    ) -> Result<RenderPassEncoder> {
        self.command_encoder
            .begin_render_pass(
                &RenderPassDescriptor {
                    label,
                    color_attachments: vec![ColorAttachment::new(
                        &self.gpu.current_context_texture_view()?,
                        LoadOp::Load,
                        StoreOp::Store,
                    )],
                    ..Default::default()
                }
                .into(),
            )
            .map_err(Into::into)
    }
}

/// Derives view-space `(near, far)` from a perspective projection
/// matrix. The renderer doesn't separately track near/far — they're
/// derived from `proj` so the camera buffer stays the single source
/// of truth. Returns sensible defaults (`(0.1, 1000.0)`) when no
/// camera matrices have been uploaded yet (first-frame race).
///
/// Recovery (right-handed glam / WebGPU NDC `z ∈ [0, 1]`):
///   `proj[2][2] = far / (near - far)`
///   `proj[3][2] = far * near / (near - far)`
/// solved as
///   `near = proj[3][2] / proj[2][2]`
///   `far  = proj[3][2] / (proj[2][2] + 1)`
fn camera_near_far_from_projection(
    last_matrices: &Option<crate::camera::CameraMatrices>,
) -> (f32, f32) {
    let Some(matrices) = last_matrices else {
        return (0.1, 1000.0);
    };
    if matrices.is_orthographic() {
        // Orthographic: no perspective divide; near/far come from
        // `proj[2][2]` and `proj[3][2]` differently. The exponential
        // froxel mapping assumes perspective; for ortho we just hand
        // back something safe and let the cull pass cover the whole
        // depth range coarsely.
        return (0.1, 1000.0);
    }
    let p22 = matrices.projection.z_axis.z;
    let p32 = matrices.projection.w_axis.z;
    // Guard against degenerate matrices.
    if p22.abs() < f32::EPSILON || (p22 + 1.0).abs() < f32::EPSILON {
        return (0.1, 1000.0);
    }
    let near = p32 / p22;
    let far = p32 / (p22 + 1.0);
    let near = near.abs().max(1e-4);
    let far = far.abs().max(near + 1e-3);
    (near, far)
}
