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
use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{BindGroupDescriptor, BindGroupEntry, BindGroupResource};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
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
        shared::material::bind_group::build_shadow_bind_group_entries,
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

    /// Dispatches the per-shader-id edge_resolve + skybox_edge_resolve
    /// and final_blend pipelines for the MSAA edge-resolve flow.
    /// Called from the renderer's frame orchestration
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
    /// neither of which lives on `RenderContext` yet (the
    /// `MaterialEdgeBuffers` allocator must be wired into the renderer's
    /// finalize-textures flow). Until that lands, this method
    /// short-circuits at the top with a tracing warn.
    pub fn render_edge_resolve(&self, ctx: &RenderContext) -> Result<()> {
        // No MSAA → no edges → nothing to dispatch.
        if ctx.anti_aliasing.msaa_sample_count.is_none() {
            return Ok(());
        }

        // final_blend is the global compositor that writes resolved edge
        // pixels back into opaque_tex — without it nothing resolves, so it
        // stays the one genuine all-or-nothing dependency. It (plus the
        // global skybox + every per-shader edge pipeline) is built reliably
        // at the LAYOUT level via `MaterialEdgePipelines::ensure_compiled`
        // (driven from `prewarm_pipelines` / `compile_material_variants`);
        // this guard only skips the brief window before that rebuild lands.
        if self.edge_pipelines.final_blend_pipeline_key.is_none() {
            warn_pipeline_not_compiled("material_opaque::edge_resolve", "final_blend");
            return Ok(());
        }

        // Edge buffer + layout uniform must exist for the dispatch
        // to bind anything. Allocated in lockstep with MSAA-on at
        // build(), so this is a defense-in-depth bail.
        let (edge_buffers, edge_layout_uniform) =
            match (ctx.material_edge_buffers, ctx.material_edge_layout_uniform) {
                (Some(b), Some(u)) => (b, u),
                _ => {
                    warn_pipeline_not_compiled(
                        "material_opaque::edge_resolve",
                        "edge buffers / layout uniform missing",
                    );
                    return Ok(());
                }
            };

        // Per-bucket-independent resolve (the old all-or-nothing gate is
        // gone). Each per-shader edge pipeline + the global skybox pipeline
        // dispatch only when resident; classify zeroes every freshly-
        // allocated edge pixel's accumulator slots, so a bucket whose
        // pipeline isn't resident this frame leaves count==0 (which
        // final_blend skips) instead of corrupting the pixel with a stale
        // previous-frame slot. Those edge pixels keep their primary-pass
        // sample-0 shading until the layout-level
        // `MaterialEdgePipelines::ensure_compiled` rebuild installs the
        // missing bucket — one never-resident bucket no longer disables
        // MSAA everywhere (the bug this replaces).
        let bucket_entries = ctx.dynamic_materials.bucket_entries_cached();

        // Build the three edge bind groups for this frame. Built on
        // every frame (not cached) — bind-group construction is cheap
        // (~few µs per group) and the cache-invalidation discipline
        // (edge buffer recreate, texture-view recreate, MSAA flip)
        // would be intricate to get right across the whole pipeline.
        //
        // `extended_shadows_group` is the shadow bind group with the
        // edge buffer + layout uniform appended (bindings 10/11); it
        // is bound at slot 3 of the edge_resolve pipeline layout in
        // place of the primary opaque shadow bind group, which is how
        // the layout fits in 4 bind groups instead of 5.
        let (extended_shadows_group, skybox_edge_group, final_blend_group) =
            self.build_edge_bind_groups(ctx, edge_buffers, edge_layout_uniform)?;

        // WebGPU validation rule: within a single compute pass, a
        // buffer used as `Indirect` (dispatch_workgroups_indirect's
        // args source) cannot also be bound as writable `Storage`.
        // The `MaterialEdgeBuffers` split (args_buffer vs data_buffer)
        // resolves this for the storage-writable accumulator side; the
        // args_buffer itself is bound only as `Storage(read)` here,
        // which is compatible with its concurrent Indirect usage as
        // the dispatch source.
        //
        // All per-shader,
        // skybox, and final_blend dispatches now live inside ONE
        // compute pass. Each separate `begin_compute_pass` on mobile
        // TBR drivers is a tile flush + barrier sync (~30 µs); with
        // N material buckets the previous shape paid N + 2 of those.
        //
        // Synchronization-scope reasoning: per-shader dispatches each
        // atomic-add into disjoint shader-bucket regions of the
        // accumulator (no cross-bucket dependency); skybox writes its
        // own slot; final_blend reads every accumulator slot and must
        // therefore land strictly after each per-shader + skybox
        // dispatch. WebGPU's automatic intra-pass barriers between
        // dispatches that share writes-to-then-reads-from storage
        // bindings handle this correctly — `final_blend`'s storage
        // read of the same buffer all per-shader passes wrote to
        // forces the barrier on its behalf.

        let (main_bind_group, lights_bind_group, texture_bind_group, _shadows_bind_group) =
            self.bind_groups.get_bind_groups()?;

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Opaque - Edge Resolve")).into(),
        ));

        // ── Per-shader-id edge_resolve dispatches ────────────────────
        // Pre-check above guarantees every bucket has a compiled
        // pipeline; the lookup is infallible here. Slots 0/1/2/3 set
        // once up front and reused — only the pipeline changes per
        // bucket. The shadow bind group at slot 3 is the extended
        // form (10 shadow bindings + edge_data + edge_layout).
        compute_pass.set_bind_group(0u32, main_bind_group, None)?;
        compute_pass.set_bind_group(1u32, lights_bind_group, None)?;
        compute_pass.set_bind_group(2u32, texture_bind_group, None)?;
        compute_pass.set_bind_group(3u32, &extended_shadows_group, None)?;
        for (bucket_index, entry) in bucket_entries.iter().enumerate() {
            // Skip buckets whose per-shader edge pipeline isn't resident
            // yet — their edge pixels keep primary-pass sample-0 shading
            // this frame (accumulator slot stays count==0, zeroed by
            // classify). Per-bucket-independent: a missing bucket no longer
            // disables MSAA for every other bucket.
            let Some(pipeline_key) = self
                .edge_pipelines
                .get_per_shader_pipeline_key(ctx.anti_aliasing, entry.shader_id)
            else {
                continue;
            };
            compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
            compute_pass.dispatch_workgroups_indirect_with_u32(
                &edge_buffers.args_buffer,
                MaterialEdgeBuffers::per_shader_args_offset(bucket_index as u32),
            );
        }

        // ── Skybox edge resolve ─────────────────────────────────────
        // Dispatches only when the global skybox pipeline is resident
        // (per-bucket-independent — no pre-check gate). The skybox pipeline
        // layout uses only group(0); the prior bindings on slots 1/2/3
        // remain set but go unused, which is permitted. If absent, skybox
        // edge pixels keep sample-0 shading (their accumulator slot stays
        // count==0, zeroed by classify).
        if let Some(skybox_pipeline_key) = self.edge_pipelines.skybox_edge_resolve_pipeline_key {
            compute_pass.set_pipeline(ctx.pipelines.compute.get(skybox_pipeline_key)?);
            compute_pass.set_bind_group(0u32, &skybox_edge_group, None)?;
            compute_pass.dispatch_workgroups_indirect_with_u32(
                &edge_buffers.args_buffer,
                MaterialEdgeBuffers::skybox_edge_args_offset(),
            );
        }

        // ── Final blend ─────────────────────────────────────────────
        // Reads every accumulator slot written above; the implicit
        // storage-barrier WebGPU inserts between dispatches that
        // share read-after-write storage bindings means this lands
        // strictly after the per-shader + skybox writes.
        if let Some(pipeline_key) = self.edge_pipelines.final_blend_pipeline_key {
            compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
            compute_pass.set_bind_group(0u32, &final_blend_group, None)?;
            compute_pass.dispatch_workgroups_indirect_with_u32(
                &edge_buffers.args_buffer,
                MaterialEdgeBuffers::final_blend_args_offset(),
            );
        }

        compute_pass.end();
        Ok(())
    }

    /// Builds the three edge bind groups for this frame. Called from
    /// `render_edge_resolve`; bind-group construction is cheap so we
    /// rebuild every frame instead of caching with invalidation logic.
    fn build_edge_bind_groups(
        &self,
        ctx: &RenderContext,
        edge_buffers: &MaterialEdgeBuffers,
        edge_layout_uniform: &web_sys::GpuBuffer,
    ) -> Result<(
        web_sys::GpuBindGroup,
        web_sys::GpuBindGroup,
        web_sys::GpuBindGroup,
    )> {
        let layouts = &self.edge_bind_group_layouts;

        // extended_shadows_group: the standard 10 shadow bindings
        // followed by edge_data (binding 10, storage RW) + edge_layout
        // (binding 11, uniform). Bound at slot 3 of the edge_resolve
        // pipeline layout in place of the primary opaque shadow bind
        // group — the fold that lets the layout fit in 4 bind groups.
        // args_buffer is NOT bound — entry counters are mirrored into
        // `edge_data`'s header so the compute stage stays under the
        // 10-storage-buffer cap.
        let mut entries_shadows = build_shadow_bind_group_entries(ctx.shadows);
        entries_shadows.push(BindGroupEntry::new(
            10,
            BindGroupResource::Buffer(BufferBinding::new(&edge_buffers.data_buffer)),
        ));
        entries_shadows.push(BindGroupEntry::new(
            11,
            BindGroupResource::Buffer(BufferBinding::new(edge_layout_uniform)),
        ));
        let descriptor_shadows = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(layouts.edge_resolve_extended_shadows_layout_key)?,
            Some("Material Edge Resolve - Extended Shadows (Group 3)"),
            entries_shadows,
        );
        let extended_shadows_group = ctx.gpu.create_bind_group(&descriptor_shadows.into());

        // Skybox-edge bind group: data + layout + camera + skybox tex
        // + sampler.
        let entries_sky = vec![
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
                BindGroupResource::Buffer(BufferBinding::new(&ctx.camera.gpu_buffer)),
            ),
            BindGroupEntry::new(
                3,
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.environment.skybox.texture_view)),
            ),
            BindGroupEntry::new(
                4,
                BindGroupResource::Sampler(&ctx.environment.skybox.sampler),
            ),
        ];
        let descriptor_sky = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(layouts.skybox_edge_group0_layout_key)?,
            Some("Material Skybox Edge Resolve - Group 0"),
            entries_sky,
        );
        let skybox_edge_group = ctx.gpu.create_bind_group(&descriptor_sky.into());

        // Final-blend bind group: data (RO) + layout + opaque storage
        // texture. Reads edge_count from `edge_data`'s header.
        let entries_final = vec![
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
                BindGroupResource::TextureView(Cow::Borrowed(&ctx.render_texture_views.opaque)),
            ),
        ];
        let descriptor_final = BindGroupDescriptor::new(
            ctx.bind_group_layouts
                .get(layouts.final_blend_group0_layout_key)?,
            Some("Material Final Blend - Group 0"),
            entries_final,
        );
        let final_blend_group = ctx.gpu.create_bind_group(&descriptor_final.into());

        Ok((extended_shadows_group, skybox_edge_group, final_blend_group))
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
