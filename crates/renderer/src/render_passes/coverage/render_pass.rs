use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        coverage::{bind_group::CoverageBindGroups, pipeline::CoveragePipelines},
        RenderPassInitContext,
    },
};

/// Coverage pass holds parallel bind-group + pipeline variants for
/// the multisampled and single-sample visibility-data shapes; the
/// render-time anti-aliasing config picks one per frame.
pub struct CoverageRenderPass {
    pub bind_groups_singlesampled: CoverageBindGroups,
    pub bind_groups_multisampled: CoverageBindGroups,
    pub pipelines_singlesampled: CoveragePipelines,
    pub pipelines_multisampled: CoveragePipelines,
}

impl CoverageRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups_singlesampled = CoverageBindGroups::new(ctx, false).await?;
        let bind_groups_multisampled = CoverageBindGroups::new(ctx, true).await?;
        let pipelines_singlesampled =
            CoveragePipelines::new(ctx, &bind_groups_singlesampled).await?;
        let pipelines_multisampled = CoveragePipelines::new(ctx, &bind_groups_multisampled).await?;
        Ok(Self {
            bind_groups_singlesampled,
            bind_groups_multisampled,
            pipelines_singlesampled,
            pipelines_multisampled,
        })
    }

    /// Dispatches the per-pixel tally. The `copy_buffer_to_buffer`
    /// that primes the readback for `mapAsync` is intentionally NOT
    /// recorded here — `render.rs` does it conditionally so the
    /// in-flight readback gate also covers the copy (writing to a
    /// pending-map buffer is a WebGPU validation error). The caller
    /// in `render.rs` zeros the counts buffer before this runs.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let _ = ctx
            .coverage_buffers
            .expect("coverage_buffers missing during coverage render");
        let msaa = ctx.anti_aliasing.msaa_sample_count.is_some();
        let (bind_groups, pipelines) = if msaa {
            (&self.bind_groups_multisampled, &self.pipelines_multisampled)
        } else {
            (
                &self.bind_groups_singlesampled,
                &self.pipelines_singlesampled,
            )
        };

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Coverage Tally")).into(),
        ));
        compute_pass.set_pipeline(ctx.pipelines.compute.get(pipelines.compute)?);
        compute_pass.set_bind_group(0, bind_groups.get_bind_group()?, None)?;
        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);
        compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));
        compute_pass.end();

        Ok(())
    }
}
