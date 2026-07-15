//! Opaque material render pass execution.
//!
//! Each bucket's specialized pipeline (the SKYBOX writer + the per-feature-set
//! material families) dispatches *indirectly* — the material classify pass
//! already produced per-bucket `(workgroup_count, 1, 1)` indirect args + a
//! per-bucket tile list the shader reads to map `workgroup_id.x →
//! (tile_x, tile_y)`. So each pipeline's dispatch only covers tiles its bucket
//! touches.
//!
//! Every registered bucket is recorded regardless of whether the scene has
//! meshes of that flavour. Indirect dispatch with `workgroup_count = 0` is a
//! documented no-op, so empty buckets pay only the dispatch-record overhead.
//! The dedicated SKYBOX bucket (index 0; `owns_skybox` → the `skybox_primary`
//! kernel — see skybox_primary.wgsl) is the one pipeline that *must* dispatch
//! even on an empty scene, since classify routes all uncovered pixels to it.

// MaterialShaderId no longer needed in this file — the dispatch loop now
// iterates registry bucket entries instead of hard-coded ids.
use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    bind_groups::BindGroupRecreateContext,
    error::Result,
    pipeline_scheduler::warn_pipeline_not_compiled,
    render::RenderContext,
    render_passes::{
        material_classify::buffers::indirect_args_offset,
        material_opaque::{
            bind_group::MaterialOpaqueBindGroups, edge_bind_group::MaterialEdgeBindGroupLayouts,
            edge_buffers::MaterialEdgeBuffers, edge_pipeline::MaterialEdgePipelines,
            pipeline::MaterialOpaquePipelines,
        },
        RenderPassInitContext,
    },
    renderable::Renderable,
};

/// Opaque material pass bind groups and pipelines.
///
/// In addition to the primary opaque pipelines (one per shader_id), the
/// MSAA edge-resolve flow adds a second tier of pipelines
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

    /// Unified-edge (U1) dispatch — the toggle-ON replacement for
    /// `render()` + `render_edge_resolve()`. Dispatches each bucket's merged
    /// `cs_shade` pipeline over its tile list (interior sample-0 → opaque_tex;
    /// edge samples → the per-material accumulator slot via edge_slot_map),
    /// then the UNCHANGED `final_blend` resolve over the edge pixels. Reuses
    /// the same accumulator + edge_slot_map + final_blend the toggle-OFF path
    /// uses, so the output is byte-identical to cs_opaque + cs_edge +
    /// skybox_primary + skybox_edge_resolve + final_blend.
    ///
    /// MSAA-only (cs_shade exists only under MSAA — there are no edges
    /// otherwise). The caller (render.rs) routes no-MSAA + toggle-on through
    /// the normal `render()` path instead.
    pub fn render_shade(&self, ctx: &RenderContext, _renderables: &[Renderable]) -> Result<()> {
        // No MSAA → no cs_shade pipelines; nothing to dispatch here. (The
        // caller should not invoke this without MSAA, but bail defensively.)
        if ctx.anti_aliasing.msaa_sample_count.is_none() {
            return Ok(());
        }

        // Edge buffers + layout uniform must exist (allocated in lockstep with
        // MSAA-on at build()). Defense-in-depth bail. The edge bind groups (built
        // by `recreate_edge`) bind the layout uniform; we only need the buffers
        // handle here (the indirect args_buffer at final-blend dispatch).
        let edge_buffers = match (ctx.material_edge_buffers, ctx.material_edge_layout_uniform) {
            (Some(b), Some(_)) => b,
            _ => {
                warn_pipeline_not_compiled(
                    "material_opaque::shade",
                    "edge buffers / layout uniform missing",
                );
                return Ok(());
            }
        };

        // The per-pixel edge-id view classify wrote (gated on MSAA) — the shade
        // group binds it, so its absence means the cached group wasn't built;
        // bail before fetching it.
        if ctx.render_texture_views.edge_id.is_none() {
            warn_pipeline_not_compiled("material_opaque::shade", "edge_id texture view missing");
            return Ok(());
        }

        let bucket_entries = ctx.dynamic_materials.bucket_entries_cached();

        // Group(3) for cs_shade (shadow bindings + edge_data@10 + edge_layout@11
        // + edge_id@12): cached + rebuilt on edge/view/shadow/AA events via
        // `MaterialOpaqueBindGroups::recreate_edge` (was rebuilt inline every
        // frame).
        let shade_group = self.bind_groups.get_edge_shade_bind_group()?;

        let (main_bind_group, lights_bind_group, texture_bind_group, _shadows_bind_group) =
            self.bind_groups.get_bind_groups()?;
        let classify_buffer = &ctx.material_classify_buffers.buffer;

        // ── Pass 1: cs_shade over every bucket's tile list ───────────────
        // Writes interior sample-0 → opaque_tex AND edge samples → the
        // accumulator (disjoint per-bucket slots, no cross-bucket dependency).
        {
            let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
                &ComputePassDescriptor::new(Some("Material Opaque - Unified Shade")).into(),
            ));
            compute_pass.set_bind_group(0u32, main_bind_group, None)?;
            compute_pass.set_bind_group(1u32, lights_bind_group, None)?;
            compute_pass.set_bind_group(2u32, texture_bind_group, None)?;
            compute_pass.set_bind_group(3u32, shade_group, None)?;
            for (bucket_index, entry) in bucket_entries.iter().enumerate() {
                let Some(pipeline_key) = self
                    .edge_pipelines
                    .get_shade_pipeline_key(ctx.anti_aliasing, entry.shader_id)
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
        }

        // ── Pass 2: final_blend resolve (UNCHANGED) ──────────────────────
        // Reads the accumulator slots cs_shade wrote, writes the weighted
        // average back to opaque_tex at each edge pixel. Separate pass (like
        // the toggle-OFF render_edge_resolve) so the opaque_tex write/write
        // across cs_shade → final_blend lands in distinct sync scopes.
        if let Some(pipeline_key) = self.edge_pipelines.final_blend_pipeline_key {
            // Cached + rebuilt on edge/view events via `recreate_edge`.
            let final_blend_group = self.bind_groups.get_edge_final_blend_bind_group()?;
            let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
                &ComputePassDescriptor::new(Some("Material Opaque - Unified Final Blend")).into(),
            ));
            compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
            compute_pass.set_bind_group(0u32, final_blend_group, None)?;
            compute_pass.dispatch_workgroups_indirect_with_u32(
                &edge_buffers.args_buffer,
                MaterialEdgeBuffers::final_blend_args_offset(),
            );
            compute_pass.end();
        } else {
            warn_pipeline_not_compiled("material_opaque::shade", "final_blend");
        }

        Ok(())
    }

    /// (Re)builds the cached edge bind groups (cs_shade group(3) + final-blend
    /// group(0)). Called by the central [`crate::bind_groups::BindGroups`]
    /// dispatcher (via `FunctionToCall::MaterialOpaqueEdge`) on the edge-group
    /// triggers — NOT every frame. The layout keys live on the pass; forward
    /// them into [`MaterialOpaqueBindGroups::recreate_edge`] (edge buffers,
    /// views, and shadows come from `ctx`). `BindGroupLayoutKey` is `Copy`, so
    /// reading the keys before the `&mut self.bind_groups` borrow keeps the
    /// field borrows disjoint.
    pub fn recreate_edge_bind_groups(&mut self, ctx: &BindGroupRecreateContext<'_>) -> Result<()> {
        let shade_key = self
            .edge_bind_group_layouts
            .shade_extended_shadows_layout_key;
        let final_blend_key = self.edge_bind_group_layouts.final_blend_group0_layout_key;
        self.bind_groups
            .recreate_edge(ctx, shade_key, final_blend_key)
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
        // compiled against (SKYBOX at index 0 + the first-party material
        // families + currently-registered dynamic materials). The SKYBOX
        // bucket at index 0 is where classify routes uncovered pixels. For
        // each bucket, dispatch its specialized opaque-compute pipeline at
        // the indirect-args offset classify wrote to.
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
