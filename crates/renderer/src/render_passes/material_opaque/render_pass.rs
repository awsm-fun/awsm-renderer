//! Opaque material render pass execution.
//!
//! Each shader_id-specialized pipeline (PBR / Unlit / Toon)
//! dispatches *indirectly* — the
//! material classify pass already produced per-bucket
//! `(workgroup_count, 1, 1)` indirect args + a per-bucket tile list
//! the shader reads to map `workgroup_id.x → (tile_x, tile_y)`. So
//! each pipeline's dispatch only covers tiles its shader_id touches.
//!
//! Three pipelines are always recorded (PBR / Unlit / Toon) regardless
//! of whether the scene has meshes of each flavour. Indirect dispatch
//! with `workgroup_count = 0` is a documented no-op, so empty buckets
//! pay only the dispatch-record overhead. The PBR pipeline is the
//! designated skybox owner — see compute.wgsl — so it's the one
//! pipeline that *must* dispatch even when no PBR meshes are present.

// MaterialShaderId no longer needed in this file — the dispatch loop now
// iterates registry bucket entries instead of hard-coded ids.
use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    pipeline_scheduler::warn_pipeline_not_compiled,
    render::RenderContext,
    render_passes::{
        material_classify::buffers::indirect_args_offset,
        material_opaque::{
            bind_group::MaterialOpaqueBindGroups,
            edge_bind_group::MaterialEdgeBindGroupLayouts,
            edge_pipeline::MaterialEdgePipelines,
            pipeline::MaterialOpaquePipelines,
        },
        RenderPassInitContext,
    },
    renderable::Renderable,
};

/// Opaque material pass bind groups and pipelines.
///
/// In addition to the primary opaque pipelines (one per shader_id), the
/// MSAA edge-resolve flow (Priority 3) adds a second tier of pipelines
/// that the dispatch loop drives: per-shader-id `edge_resolve`, the
/// global `skybox_edge_resolve`, and the global `final_blend`
/// compositor. Their compile lifecycle is scheduler-managed (lazy);
/// dispatches that find a Pending pipeline silently skip via the
/// `warn_pipeline_not_compiled` helper.
pub struct MaterialOpaqueRenderPass {
    pub bind_groups: MaterialOpaqueBindGroups,
    pub pipelines: MaterialOpaquePipelines,
    /// Pipeline cache for the per-shader-id edge_resolve + the two
    /// global edge-resolve compositor pipelines. Populated by the
    /// scheduler as the materials' edge_resolve compile futures
    /// resolve.
    pub edge_pipelines: MaterialEdgePipelines,
    /// Cached bind-group layouts for the edge-resolve pipelines.
    /// Allocated up-front (cheap — just inserts into the shared
    /// `BindGroupLayouts` cache); reused across every edge-resolve
    /// pipeline compile.
    pub edge_bind_group_layouts: MaterialEdgeBindGroupLayouts,
}

impl MaterialOpaqueRenderPass {
    /// Creates the opaque material render pass resources.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = MaterialOpaqueBindGroups::new(ctx).await?;
        let pipelines = MaterialOpaquePipelines::new(ctx, &bind_groups).await?;
        let edge_bind_group_layouts = MaterialEdgeBindGroupLayouts::new(ctx)?;
        let edge_pipelines = MaterialEdgePipelines::new();

        Ok(Self {
            bind_groups,
            pipelines,
            edge_pipelines,
            edge_bind_group_layouts,
        })
    }

    /// Rebuilds bind groups and pipelines after texture pool changes.
    pub async fn texture_pool_changed(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
    ) -> Result<()> {
        self.bind_groups = self.bind_groups.clone_because_texture_pool_changed(ctx)?;
        self.pipelines = MaterialOpaquePipelines::new(ctx, &self.bind_groups).await?;
        // Edge resolve pipelines are scheduler-managed — they'll
        // recompile against the new texture pool the next time a
        // material is registered, which kicks off the same scheduler
        // batch path. Bind-group layouts don't depend on texture pool
        // shape, so they're left alone.
        Ok(())
    }

    /// Dispatches the per-shader-id edge_resolve + skybox_edge_resolve
    /// + final_blend pipelines for the MSAA edge-resolve flow
    /// (Priority 3). Called from the renderer's frame orchestration
    /// after the primary opaque dispatch.
    ///
    /// **Lazy-pool semantics:** any pipeline whose typed-key accessor
    /// returns `None` is silently skipped via
    /// `pipeline_scheduler::warn_pipeline_not_compiled`. The primary
    /// opaque pass already wrote non-edge pixels; the edge contributions
    /// stay as transparent-black accumulator slots until the matching
    /// edge_resolve pipeline finishes compiling.
    ///
    /// **Bind-group binding:** the edge dispatches need access to the
    /// edge buffer (read-write storage) + the edge-layout uniform —
    /// neither of which lives on `RenderContext` yet (Stage 3.7 wires
    /// the `MaterialEdgeBuffers` allocator into the renderer's
    /// finalize-textures flow). Until that lands, this method
    /// short-circuits at the top with a tracing warn.
    pub fn render_edge_resolve(&self, ctx: &RenderContext) -> Result<()> {
        // No MSAA → no edges → nothing to dispatch.
        if ctx.anti_aliasing.msaa_sample_count.is_none() {
            return Ok(());
        }

        // The per-pass typed-key accessor returns None if the global
        // final_blend pipeline hasn't been compiled yet — we use it as
        // the gate (all three edge-resolve pipelines are submitted
        // together in one scheduler batch on first opaque material
        // registration, so they're either all ready or all pending).
        if self.edge_pipelines.final_blend_pipeline_key.is_none() {
            warn_pipeline_not_compiled("material_opaque::edge_resolve", "final_blend");
            return Ok(());
        }

        // Per-shader-id edge_resolve dispatches.
        let bucket_entries = ctx.dynamic_materials.bucket_entries_cached();
        for entry in bucket_entries.iter() {
            let Some(_pipeline_key) = self
                .edge_pipelines
                .get_per_shader_pipeline_key(ctx.anti_aliasing, entry.shader_id)
            else {
                let id_label = format!("{:?}", entry.shader_id);
                warn_pipeline_not_compiled(
                    "material_opaque::edge_resolve::per_shader",
                    id_label.as_str(),
                );
                continue;
            };
            // Bind group + dispatch wiring lands with Stage 3.7's
            // edge-buffer allocator. At this commit we have the
            // pipeline cache populated but no edge buffer to bind, so
            // the dispatch is a no-op placeholder. Visual output
            // identical to the previous commit (edges render as
            // transparent black) until 3.7 lands the buffer + bind
            // group + dispatch call.
            let _ = _pipeline_key;
        }

        // Skybox edge resolve + final blend dispatches — same shape;
        // gated by the same all-or-nothing scheduler batch. Wired
        // alongside the edge-buffer allocator in Stage 3.7.

        Ok(())
    }

    /// Executes the opaque material pass.
    ///
    /// `renderables` is no longer consulted for dispatch — classify
    /// determines the per-bucket tile lists. It's still in the
    /// signature so the renderable list keeps flowing through the
    /// render-graph API; future work may use it for skinning-skip /
    /// material-LOD inputs.
    pub fn render(&self, ctx: &RenderContext, _renderables: &[Renderable]) -> Result<()> {
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Opaque Pass")).into(),
        ));

        let (main_bind_group, lights_bind_group, texture_bind_group, shadows_bind_group) =
            self.bind_groups.get_bind_groups()?;

        compute_pass.set_bind_group(0u32, main_bind_group, None)?;
        compute_pass.set_bind_group(1u32, lights_bind_group, None)?;
        compute_pass.set_bind_group(2u32, texture_bind_group, None)?;
        compute_pass.set_bind_group(3u32, shadows_bind_group, None)?;

        let classify_buffer = &ctx.material_classify_buffers.buffer;

        // Iterate the same bucket list the classify shader was
        // compiled against (first-party + currently-registered
        // dynamic materials). PBR is at index 0 by convention so
        // skybox routing lands cleanly. For each bucket, dispatch
        // its specialized opaque-compute pipeline at the indirect-
        // args offset classify wrote to.
        //
        // Reads from the registry's cached slice — refreshed on
        // register / unregister, so no per-frame alloc + sort.
        let bucket_entries = ctx.dynamic_materials.bucket_entries_cached();
        for (bucket_index, entry) in bucket_entries.iter().enumerate() {
            let Some(pipeline_key) = self
                .pipelines
                .get_compute_pipeline_key(ctx.anti_aliasing, entry.shader_id)
            else {
                continue;
            };
            compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
            compute_pass.dispatch_workgroups_indirect_with_u32(
                classify_buffer,
                indirect_args_offset(bucket_index as u32),
            );
        }

        compute_pass.end();

        Ok(())
    }
}
