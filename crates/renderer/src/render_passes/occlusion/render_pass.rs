//! Occlusion-cull render pass execution — Cluster 7.2 / plan §16.7
//! Phase 1.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        occlusion::{bind_group::OcclusionBindGroups, pipeline::OcclusionPipelines},
        RenderPassInitContext,
    },
};

pub struct OcclusionRenderPass {
    pub bind_groups: OcclusionBindGroups,
    pub pipelines: OcclusionPipelines,
}

impl OcclusionRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = OcclusionBindGroups::new(ctx).await?;
        let pipelines = OcclusionPipelines::new(ctx, &bind_groups).await?;
        Ok(Self {
            bind_groups,
            pipelines,
        })
    }

    /// Dispatches the cull compute. `instance_count` is the number of
    /// active instances written into the GPU buffer this frame —
    /// dispatch is rounded up to the workgroup size (64) so the
    /// per-thread bounds check in the shader handles the leftover.
    pub fn render(&self, ctx: &RenderContext, instance_count: u32) -> Result<()> {
        if instance_count == 0 {
            return Ok(());
        }
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Occlusion Cull")).into(),
        ));
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.cull)?);
        compute_pass.set_bind_group(0, self.bind_groups.get_bind_group()?, None)?;
        let workgroups = instance_count.div_ceil(64);
        compute_pass.dispatch_workgroups(workgroups, Some(1), Some(1));
        compute_pass.end();
        Ok(())
    }
}
