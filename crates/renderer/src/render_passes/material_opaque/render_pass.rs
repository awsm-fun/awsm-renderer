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

use awsm_materials::MaterialShaderId;
use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        material_classify::buffers::indirect_args_offset,
        material_opaque::{
            bind_group::MaterialOpaqueBindGroups, pipeline::MaterialOpaquePipelines,
        },
        RenderPassInitContext,
    },
    renderable::Renderable,
};

/// Opaque material pass bind groups and pipelines.
pub struct MaterialOpaqueRenderPass {
    pub bind_groups: MaterialOpaqueBindGroups,
    pub pipelines: MaterialOpaquePipelines,
}

impl MaterialOpaqueRenderPass {
    /// Creates the opaque material render pass resources.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = MaterialOpaqueBindGroups::new(ctx).await?;
        let pipelines = MaterialOpaquePipelines::new(ctx, &bind_groups).await?;

        Ok(Self {
            bind_groups,
            pipelines,
        })
    }

    /// Rebuilds bind groups and pipelines after texture pool changes.
    pub async fn texture_pool_changed(
        &mut self,
        ctx: &mut RenderPassInitContext<'_>,
    ) -> Result<()> {
        self.bind_groups = self.bind_groups.clone_because_texture_pool_changed(ctx)?;
        self.pipelines = MaterialOpaquePipelines::new(ctx, &self.bind_groups).await?;

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

        // PBR — also owns skybox, so always dispatched.
        // Bucket index 0 == PBR; classify wrote its tiles starting at
        // `pbr_offset` and its workgroup count to `args_pbr.x`.
        for (shader_id, bucket_index) in [
            (MaterialShaderId::PBR, 0u32),
            (MaterialShaderId::UNLIT, 1u32),
            (MaterialShaderId::TOON, 2u32),
            (MaterialShaderId::FLIPBOOK, 3u32),
        ] {
            let Some(pipeline_key) = self
                .pipelines
                .get_compute_pipeline_key(ctx.anti_aliasing, shader_id)
            else {
                continue;
            };
            compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
            compute_pass.dispatch_workgroups_indirect_with_u32(
                classify_buffer,
                indirect_args_offset(bucket_index),
            );
        }

        compute_pass.end();

        Ok(())
    }
}
