//! Material prep render pass execution (Plan B,
//! docs/plans/deferred-shared-prep-pass.md).
//!
//! A static compute pass (mirrors [`crate::render_passes::light_culling`]): runs
//! once per pixel over the visibility buffer, after classify and before
//! per-material shading, materializing the material-INDEPENDENT geometry-pool
//! attributes (UV0 + vertex color) into the prep output storage textures.
//!
//! **Lazy-pool, active-branch-only.** The prep megashader is the single most
//! expensive compile in the renderer (~1s per pipeline on a cold cache), so
//! only the pipelines the LIVE config can dispatch are compiled at boot,
//! through the cross-renderer pool (`describe → from_resolved`, like the
//! geometry/classify passes):
//! - `cs_prep` for the active MSAA-geometry branch only;
//! - `cs_prep_edge` only under MSAA on a device with edge-resolve support;
//! - the denoise blur pair only while `ShadowsConfig::denoise` is on.
//!
//! The other MSAA branch compiles on the first `set_anti_aliasing` flip (which
//! is already a modal-covered, material-recompiling operation); a runtime
//! denoise enable compiles the blur pair through the same config-ensure path.
//!
//! The prep pass itself is unconditional — always constructed and dispatched.
//! The opaque deferred path reads its outputs.

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{BindGroupDescriptor, BindGroupEntry, BindGroupResource};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    pipeline_layouts::PipelineLayoutCacheKey,
    pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey},
    render::RenderContext,
    render_passes::{
        material_opaque::edge_buffers::MaterialEdgeBuffers,
        material_prep::{
            bind_group::MaterialPrepBindGroups,
            buffers::EdgeShadowBuffer,
            shader::cache_key::{ShaderCacheKeyMaterialPrep, ShaderCacheKeyShadowBlur},
        },
        RenderPassInitContext,
    },
};

/// Which prep pipeline a pooled cache-key slot resolves to. Parallel to the
/// `pipeline_cache_keys` vec in [`MaterialPrepPrewarmDescriptors`] — the same
/// routing trick the classify/HZB passes use so `merge_resolved` can drop each
/// compiled key into the right [`MaterialPrepPipelines`] field.
#[derive(Clone, Copy, Debug)]
pub enum PrepPipelineSlot {
    /// `cs_prep` for one MSAA-geometry branch.
    Main { multisampled: bool },
    /// `cs_prep_edge` (multisampled main layout + edge group(3)). MSAA-only.
    Edge,
    /// `cs_blur_h` for one MSAA-geometry branch.
    BlurH { multisampled: bool },
    /// `cs_blur_v` for one MSAA-geometry branch.
    BlurV { multisampled: bool },
}

/// Phase-2 output of [`MaterialPrepPipelines::build_descriptors`]: the pooled
/// compute cache keys + the slot each resolves to.
pub struct MaterialPrepPrewarmDescriptors {
    pub pipeline_cache_keys: Vec<ComputePipelineCacheKey>,
    pub slots: Vec<PrepPipelineSlot>,
}

/// The prep pass's compiled pipelines, branch-keyed. Every field is an
/// `Option`: only the live config's branch is compiled at boot (through the
/// cross-renderer pool); the other MSAA branch fills on the first
/// `set_anti_aliasing` flip, and the blur pair only exists while denoise is
/// configured on. `merge_resolved` preserves already-populated fields, so a
/// flip-back is a cache hit, never a loss.
#[derive(Default)]
pub struct MaterialPrepPipelines {
    multisampled: Option<ComputePipelineKey>,
    singlesampled: Option<ComputePipelineKey>,
    edge: Option<ComputePipelineKey>,
    blur_h_multisampled: Option<ComputePipelineKey>,
    blur_h_singlesampled: Option<ComputePipelineKey>,
    blur_v_multisampled: Option<ComputePipelineKey>,
    blur_v_singlesampled: Option<ComputePipelineKey>,
}

impl MaterialPrepPipelines {
    /// The shader cache keys the ACTIVE config's prep pipelines need — pooled
    /// into the cross-renderer `Shaders::ensure_keys` batch by
    /// `RenderPasses::describe_shaders`. One prep megashader module covers
    /// both `cs_prep` and `cs_prep_edge` (same cache key); the blur module is
    /// separate and only needed while denoise is on.
    pub fn shader_cache_keys(
        multisampled_geometry: bool,
        prep_config: &crate::render_passes::material_prep::PrepPassConfig,
    ) -> Vec<crate::shaders::ShaderCacheKey> {
        let msaa_sample_count = if multisampled_geometry { Some(4) } else { None };
        let mut keys: Vec<crate::shaders::ShaderCacheKey> = vec![ShaderCacheKeyMaterialPrep {
            msaa_sample_count,
            max_shadow_casters: prep_config.clamped_k(),
            sscs_enabled: prep_config.sscs_enabled,
            sscs_step_count: prep_config.sscs_step_count,
        }
        .into()];
        if prep_config.denoise {
            keys.push(
                ShaderCacheKeyShadowBlur {
                    msaa_sample_count,
                    max_shadow_casters: prep_config.clamped_k(),
                }
                .into(),
            );
        }
        keys
    }

    /// Build the pooled pipeline cache keys for ONE MSAA-geometry branch of
    /// the current `ctx.prep_config`. Shared by the boot orchestrator (active
    /// branch) and `set_anti_aliasing` (incoming branch). Sync apart from
    /// cache-hit `shaders.get_key` awaits — the shader modules are already
    /// warm from the pooled shader batch.
    ///
    /// `edge_resolve_enabled` gates `cs_prep_edge` (multisampled branch only —
    /// its layout binds the multisampled main BGL); the blur pair is gated on
    /// `prep_config.denoise`.
    pub async fn build_descriptors_for_config(
        ctx: &mut RenderPassInitContext<'_>,
        bind_groups: &MaterialPrepBindGroups,
        multisampled_geometry: bool,
        edge_resolve_enabled: bool,
    ) -> Result<MaterialPrepPrewarmDescriptors> {
        let mut pipeline_cache_keys = Vec::new();
        let mut slots = Vec::new();

        pipeline_cache_keys.push(main_cache_key(ctx, bind_groups, multisampled_geometry).await?);
        slots.push(PrepPipelineSlot::Main {
            multisampled: multisampled_geometry,
        });

        if multisampled_geometry && edge_resolve_enabled {
            pipeline_cache_keys.push(edge_cache_key(ctx, bind_groups).await?);
            slots.push(PrepPipelineSlot::Edge);
        }

        if ctx.prep_config.denoise {
            pipeline_cache_keys.push(
                blur_cache_key(ctx, bind_groups, multisampled_geometry, "cs_blur_h").await?,
            );
            slots.push(PrepPipelineSlot::BlurH {
                multisampled: multisampled_geometry,
            });
            pipeline_cache_keys.push(
                blur_cache_key(ctx, bind_groups, multisampled_geometry, "cs_blur_v").await?,
            );
            slots.push(PrepPipelineSlot::BlurV {
                multisampled: multisampled_geometry,
            });
        }

        Ok(MaterialPrepPrewarmDescriptors {
            pipeline_cache_keys,
            slots,
        })
    }

    /// Route resolved pipeline keys into their branch fields. Preserves
    /// already-populated fields not mentioned in `slots` (the lazy-branch
    /// merge contract, mirroring `GeometryPipelines::merge_resolved`).
    pub fn merge_resolved(&mut self, slots: &[PrepPipelineSlot], keys: Vec<ComputePipelineKey>) {
        debug_assert_eq!(slots.len(), keys.len());
        for (slot, key) in slots.iter().zip(keys) {
            match slot {
                PrepPipelineSlot::Main { multisampled: true } => self.multisampled = Some(key),
                PrepPipelineSlot::Main {
                    multisampled: false,
                } => self.singlesampled = Some(key),
                PrepPipelineSlot::Edge => self.edge = Some(key),
                PrepPipelineSlot::BlurH { multisampled: true } => {
                    self.blur_h_multisampled = Some(key)
                }
                PrepPipelineSlot::BlurH {
                    multisampled: false,
                } => self.blur_h_singlesampled = Some(key),
                PrepPipelineSlot::BlurV { multisampled: true } => {
                    self.blur_v_multisampled = Some(key)
                }
                PrepPipelineSlot::BlurV {
                    multisampled: false,
                } => self.blur_v_singlesampled = Some(key),
            }
        }
    }

    /// Whether everything the given config needs is already compiled — the
    /// `set_anti_aliasing` / config-ensure guard that makes a flip-back a
    /// no-op (mirrors `GeometryPipelines::has_branch_for`).
    pub fn has_branch_for(
        &self,
        multisampled_geometry: bool,
        edge_resolve_enabled: bool,
        denoise: bool,
    ) -> bool {
        let main = if multisampled_geometry {
            self.multisampled.is_some()
        } else {
            self.singlesampled.is_some()
        };
        let edge = !(multisampled_geometry && edge_resolve_enabled) || self.edge.is_some();
        let blur = !denoise
            || if multisampled_geometry {
                self.blur_h_multisampled.is_some() && self.blur_v_multisampled.is_some()
            } else {
                self.blur_h_singlesampled.is_some() && self.blur_v_singlesampled.is_some()
            };
        main && edge && blur
    }
}

/// Material prep pass bind groups + the branch-keyed compiled pipelines.
pub struct MaterialPrepRenderPass {
    pub bind_groups: MaterialPrepBindGroups,
    pub pipelines: MaterialPrepPipelines,
    /// Stage 5b-shadow: the compact per-edge-sample shadow texture cs_prep_edge
    /// writes + cs_edge reads. `Some` only under MSAA on a device with
    /// edge-resolve support (~8 MB — allocated on the MSAA-on flip, dropped on
    /// MSAA-off, mirroring `material_edge_buffers`).
    pub edge_shadow: Option<EdgeShadowBuffer>,
    /// One-shot "blur pipelines not compiled yet" warning latch (mirrors the
    /// shadow pass's `warn_pipeline_not_compiled` discipline): a runtime
    /// denoise enable is only picked up by the next config-ensure/commit, so
    /// the frames in between skip the blur with a single warn instead of
    /// erroring the whole frame.
    blur_warned: std::cell::Cell<bool>,
}

impl MaterialPrepRenderPass {
    /// Assemble the pass from staged parts (bind groups from
    /// `describe_shaders`, pipelines merged from the cross-renderer pool,
    /// edge-shadow texture allocated by `describe_pipelines` when gated in).
    pub fn from_resolved(
        bind_groups: MaterialPrepBindGroups,
        pipelines: MaterialPrepPipelines,
        edge_shadow: Option<EdgeShadowBuffer>,
    ) -> Self {
        Self {
            bind_groups,
            pipelines,
            edge_shadow,
            blur_warned: std::cell::Cell::new(false),
        }
    }

    /// Stage 5b-shadow: allocate the compact edge-shadow texture if absent
    /// (the MSAA-on flip path). No-op when already allocated.
    pub fn ensure_edge_shadow(
        &mut self,
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        max_edge_budget: u32,
        layers: u32,
    ) -> Result<()> {
        if self.edge_shadow.is_none() {
            self.edge_shadow = Some(EdgeShadowBuffer::new(gpu, max_edge_budget, layers)?);
        }
        Ok(())
    }

    /// Stage 5b-shadow: drop the compact edge-shadow texture (the MSAA-off
    /// flip path — mirrors `material_edge_buffers` teardown; the ~8 MB
    /// texture is pure waste while single-sampled).
    pub fn drop_edge_shadow(&mut self) {
        self.edge_shadow = None;
    }

    /// Dispatches the prep shader: one workgroup per 8×8 tile of the
    /// visibility buffer. Picks the pipeline variant matching the live MSAA
    /// state — which MUST be compiled (boot compiles the active branch;
    /// `set_anti_aliasing` compiles the incoming one before flipping).
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let pipeline_key = if ctx.anti_aliasing.msaa_sample_count.is_some() {
            self.pipelines.multisampled
        } else {
            self.pipelines.singlesampled
        };
        let pipeline_key = pipeline_key.ok_or(crate::error::AwsmError::PipelineVariantNotCompiled(
            "material prep pipeline for the live MSAA branch (set_anti_aliasing must compile the incoming branch before flipping)",
        ))?;
        let pipeline = ctx.pipelines.compute.get(pipeline_key)?;
        let bind_group = self.bind_groups.get_bind_group()?;
        let lights_bind_group = self.bind_groups.get_lights_bind_group()?;
        let shadows_bind_group = self.bind_groups.get_shadows_bind_group()?;

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Prep Pass")).into(),
        ));
        compute_pass.set_pipeline(pipeline);
        compute_pass.set_bind_group(0, bind_group, None)?;
        compute_pass.set_bind_group(1, lights_bind_group, None)?;
        compute_pass.set_bind_group(2, shadows_bind_group, None)?;

        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);
        compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));

        compute_pass.end();
        Ok(())
    }

    /// Stage 5b-shadow: resize the compact edge-shadow texture to a new edge
    /// budget (mirrors `MaterialEdgeBuffers::set_max_edge_budget`). No-op when the
    /// budget is unchanged or this prep pass has no edge texture (non-MSAA).
    /// Caller marks the dependent bind groups (opaque main binding 27) dirty.
    pub fn set_max_edge_budget(
        &mut self,
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        new_budget: u32,
    ) -> Result<bool> {
        let Some(existing) = self.edge_shadow.as_ref() else {
            return Ok(false);
        };
        if existing.max_edge_budget == new_budget.max(1) {
            return Ok(false);
        }
        let layers = existing.layers;
        self.edge_shadow = Some(EdgeShadowBuffer::new(gpu, new_budget, layers)?);
        Ok(true)
    }

    /// Stage 5b-shadow: dispatch `cs_prep_edge` — fills the compact per-edge-
    /// sample shadow texture so the MSAA `cs_edge` can read it instead of inline
    /// sampling shadow maps. Indirect over `edge_count` (reuses the
    /// `final_blend_args` DispatchIndirectArgs cell, already sized for all
    /// edges). Inserted between `cs_prep` and the opaque pass; only effective
    /// under MSAA (the pipeline + texture are `None` otherwise). No-op when the
    /// edge buffers / layout uniform aren't allocated (non-MSAA).
    pub fn render_edge(&self, ctx: &RenderContext) -> Result<()> {
        // cs_prep_edge is MSAA-only: its pipeline layout binds the *multisampled*
        // prep main BGL at group(0), so it must never run while the live prep main
        // bind group is single-sampled (that mismatch invalidates the whole frame's
        // command buffer). `set_anti_aliasing` now tears `material_edge_buffers`
        // down on an MSAA on→off flip, so the buffer-presence guard below already
        // no-ops when MSAA is off — but this pass keys off the live MSAA state
        // directly rather than trusting that invariant, mirroring the classify
        // pass's `if msaa` discipline. Defense-in-depth: an MSAA-only pass enforces
        // its own contract regardless of edge-buffer lifecycle.
        if ctx.anti_aliasing.msaa_sample_count.is_none() {
            return Ok(());
        }
        // Edge buffers exist ⟺ MSAA-on AND the device supports edge resolve
        // (see `set_anti_aliasing`) — absent means this frame legitimately has
        // no edge-resolve path, so skip.
        let (edge_buffers, edge_layout_uniform) =
            match (ctx.material_edge_buffers, ctx.material_edge_layout_uniform) {
                (Some(b), Some(u)) => (b, u),
                _ => return Ok(()),
            };
        // Past this point the edge path IS live — a missing pipeline/texture is
        // a broken invariant (cs_edge reads the compact texture this dispatch
        // fills; skipping silently would shade edges with garbage shadows).
        let edge_pipeline_key = self.pipelines.edge.ok_or(
            crate::error::AwsmError::PipelineVariantNotCompiled(
                "cs_prep_edge (edge buffers live but the edge prep pipeline never compiled)",
            ),
        )?;
        let edge_shadow = self.edge_shadow.as_ref().ok_or(
            crate::error::AwsmError::PipelineVariantNotCompiled(
                "edge-shadow texture (edge buffers live but the compact texture never allocated)",
            ),
        )?;
        let edge_bgl_key = match self.bind_groups.edge_bind_group_layout_key {
            Some(k) => k,
            None => return Ok(()),
        };

        let pipeline = ctx.pipelines.compute.get(edge_pipeline_key)?;
        let bind_group = self.bind_groups.get_bind_group()?;
        let lights_bind_group = self.bind_groups.get_lights_bind_group()?;
        let shadows_bind_group = self.bind_groups.get_shadows_bind_group()?;

        // group(3) built fresh each frame (cheap; mirrors the opaque edge-resolve
        // pass): edge_data (RO) + edge_layout + edge_shadow_out (storage write).
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&edge_buffers.data_buffer)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(edge_layout_uniform)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::TextureView(Cow::Borrowed(&edge_shadow.storage_view)),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(edge_bgl_key)?,
            Some("Material Prep Edge - Group 3"),
            entries,
        );
        let edge_bind_group = ctx.gpu.create_bind_group(&descriptor.into());

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Prep Edge Pass")).into(),
        ));
        compute_pass.set_pipeline(pipeline);
        compute_pass.set_bind_group(0, bind_group, None)?;
        compute_pass.set_bind_group(1, lights_bind_group, None)?;
        compute_pass.set_bind_group(2, shadows_bind_group, None)?;
        compute_pass.set_bind_group(3, &edge_bind_group, None)?;
        // Indirect over edge_count via the final_blend_args cell (workgroup_size
        // 64; the cell's workgroup_count_x = ceil(edge_count / 64), set by
        // classify — already sized for all edges).
        compute_pass.dispatch_workgroups_indirect_with_u32(
            &edge_buffers.args_buffer,
            MaterialEdgeBuffers::final_blend_args_offset(),
        );
        compute_pass.end();
        Ok(())
    }

    /// Optional shadow-visibility denoise blur. A single separable, edge-aware
    /// (depth-stopped) screen-space pass over `prep_shadow_visibility`: H writes
    /// the temp, V writes back, so the opaque reader's binding never changes.
    /// Smooths the residual soft/PCSS penumbra speckle for ALL shadowed lights
    /// at once (cost independent of light count). Skipped entirely when the
    /// runtime `ShadowsConfig::denoise` toggle is off. Inserted between
    /// `cs_prep`/`cs_prep_edge` and the opaque pass (compute passes in one
    /// encoder are ordered, so the write→read is safe with no explicit barrier).
    pub fn render_blur(&self, ctx: &RenderContext) -> Result<()> {
        if !ctx.shadows.config().denoise {
            return Ok(());
        }
        // Nothing casts → `prep_shadow_visibility` is all-1.0; blurring it is a
        // no-op. Skip the two full-screen dispatches (matches how the shadow
        // generation pass itself short-circuits on `any_active()`).
        if !ctx.shadows.any_active() {
            return Ok(());
        }
        let msaa = ctx.anti_aliasing.msaa_sample_count.is_some();
        let keys = if msaa {
            (
                self.pipelines.blur_h_multisampled,
                self.pipelines.blur_v_multisampled,
            )
        } else {
            (
                self.pipelines.blur_h_singlesampled,
                self.pipelines.blur_v_singlesampled,
            )
        };
        let (h_key, v_key) = match keys {
            (Some(h), Some(v)) => (h, v),
            _ => {
                // Denoise was enabled at runtime and the config-ensure that
                // compiles the blur pair hasn't run yet — skip the blur (the
                // un-denoised visibility is still correct) with a one-shot
                // warn, mirroring the shadow pass's not-compiled discipline.
                if !self.blur_warned.replace(true) {
                    tracing::warn!(
                        "shadow denoise blur pipelines not compiled for the live MSAA branch — \
                         skipping the blur until the next commit_load / config ensure"
                    );
                }
                return Ok(());
            }
        };
        self.blur_warned.set(false);
        let h_pipeline = ctx.pipelines.compute.get(h_key)?;
        let v_pipeline = ctx.pipelines.compute.get(v_key)?;
        let h_bind_group = self.bind_groups.get_blur_h_bind_group()?;
        let v_bind_group = self.bind_groups.get_blur_v_bind_group()?;

        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);

        // Horizontal: prep_shadow_visibility → temp.
        {
            let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
                &ComputePassDescriptor::new(Some("Shadow Denoise Blur H")).into(),
            ));
            compute_pass.set_pipeline(h_pipeline);
            compute_pass.set_bind_group(0, h_bind_group, None)?;
            compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));
            compute_pass.end();
        }
        // Vertical: temp → prep_shadow_visibility (back in place).
        {
            let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
                &ComputePassDescriptor::new(Some("Shadow Denoise Blur V")).into(),
            ));
            compute_pass.set_pipeline(v_pipeline);
            compute_pass.set_bind_group(0, v_bind_group, None)?;
            compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));
            compute_pass.end();
        }
        Ok(())
    }
}

/// Derives the `cs_prep` pipeline CACHE KEY for one MSAA-geometry variant —
/// no compile; the key joins the cross-renderer pool. The `shaders.get_key`
/// await is a cache hit (the pooled shader batch ran first).
async fn main_cache_key(
    ctx: &mut RenderPassInitContext<'_>,
    bind_groups: &MaterialPrepBindGroups,
    multisampled_geometry: bool,
) -> Result<ComputePipelineCacheKey> {
    let bgl_key = if multisampled_geometry {
        bind_groups.multisampled_bind_group_layout_key
    } else {
        bind_groups.singlesampled_bind_group_layout_key
    };
    let pipeline_layout_key = ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![
            bgl_key,
            bind_groups.lights_bind_group_layout_key,
            bind_groups.shadows_bind_group_layout_key,
        ]),
    )?;
    let shader_key = ctx
        .shaders
        .get_key(
            ctx.gpu,
            ShaderCacheKeyMaterialPrep {
                msaa_sample_count: if multisampled_geometry { Some(4) } else { None },
                max_shadow_casters: ctx.prep_config.clamped_k(),
                sscs_enabled: ctx.prep_config.sscs_enabled,
                sscs_step_count: ctx.prep_config.sscs_step_count,
            },
        )
        .await?;
    Ok(ComputePipelineCacheKey::new(shader_key, pipeline_layout_key).with_entry_point("cs_prep"))
}

/// Stage 5b-shadow: derives the `cs_prep_edge` pipeline CACHE KEY (MSAA only).
/// Shares the MSAA prep shader module (same cache key as the multisampled
/// `cs_prep`); its pipeline layout adds group(3) = the edge layout (edge_data +
/// edge_layout + edge_shadow_out) on top of the multisampled main + lights +
/// shadows groups.
async fn edge_cache_key(
    ctx: &mut RenderPassInitContext<'_>,
    bind_groups: &MaterialPrepBindGroups,
) -> Result<ComputePipelineCacheKey> {
    let edge_bgl_key = bind_groups
        .edge_bind_group_layout_key
        .expect("edge bind group layout must exist under MSAA");
    let pipeline_layout_key = ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![
            bind_groups.multisampled_bind_group_layout_key,
            bind_groups.lights_bind_group_layout_key,
            bind_groups.shadows_bind_group_layout_key,
            edge_bgl_key,
        ]),
    )?;
    let shader_key = ctx
        .shaders
        .get_key(
            ctx.gpu,
            ShaderCacheKeyMaterialPrep {
                msaa_sample_count: Some(4),
                max_shadow_casters: ctx.prep_config.clamped_k(),
                sscs_enabled: ctx.prep_config.sscs_enabled,
                sscs_step_count: ctx.prep_config.sscs_step_count,
            },
        )
        .await?;
    Ok(
        ComputePipelineCacheKey::new(shader_key, pipeline_layout_key)
            .with_entry_point("cs_prep_edge"),
    )
}

/// Derives one shadow-denoise blur pipeline CACHE KEY (`entry_point` =
/// `cs_blur_h` or `cs_blur_v`) for one MSAA-geometry variant. Both entry
/// points share the one blur shader module (same `ShaderCacheKeyShadowBlur`);
/// only the pipeline's entry point + the H/V bind group differ. Pipeline
/// layout = the single blur bind group at group(0).
async fn blur_cache_key(
    ctx: &mut RenderPassInitContext<'_>,
    bind_groups: &MaterialPrepBindGroups,
    multisampled_geometry: bool,
    entry_point: &str,
) -> Result<ComputePipelineCacheKey> {
    let bgl_key = if multisampled_geometry {
        bind_groups.blur_multisampled_bind_group_layout_key
    } else {
        bind_groups.blur_singlesampled_bind_group_layout_key
    };
    let pipeline_layout_key = ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![bgl_key]),
    )?;
    let shader_key = ctx
        .shaders
        .get_key(
            ctx.gpu,
            ShaderCacheKeyShadowBlur {
                msaa_sample_count: if multisampled_geometry { Some(4) } else { None },
                max_shadow_casters: ctx.prep_config.clamped_k(),
            },
        )
        .await?;
    Ok(ComputePipelineCacheKey::new(shader_key, pipeline_layout_key).with_entry_point(entry_point))
}
