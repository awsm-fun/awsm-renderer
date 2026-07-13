use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        material_decal::classify::{
            bind_group::DecalClassifyBindGroups, pipeline::DecalClassifyPipelines,
        },
        RenderPassInitContext,
    },
};

pub struct DecalClassifyRenderPass {
    pub bind_groups: DecalClassifyBindGroups,
    /// Content-lazy: `None` until `ensure_config_pipelines` compiles it at the
    /// first commit with a live decal. Dispatch skips while missing (the decal
    /// pass's own `pipelines` gate short-circuits before classify anyway).
    pub pipelines: Option<DecalClassifyPipelines>,
}

impl DecalClassifyRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = DecalClassifyBindGroups::new(ctx).await?;
        let pipelines = Some(DecalClassifyPipelines::new(ctx, &bind_groups).await?);
        Ok(Self {
            bind_groups,
            pipelines,
        })
    }

    /// Dispatches one workgroup of 64 threads per `MAX_DECAL_COUNT/64`
    /// decals. The shader's per-thread bounds check handles the
    /// leftover when the active count isn't a multiple of 64.
    pub fn render(&self, ctx: &RenderContext, decal_count: u32) -> Result<()> {
        if decal_count == 0 {
            return Ok(());
        }
        // Content-lazy: not compiled yet (between a runtime decal insert and
        // the next commit) — skip; the decal simply doesn't shade this frame.
        let Some(pipelines) = self.pipelines.as_ref() else {
            return Ok(());
        };
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Decal Classify Pass")).into(),
        ));
        compute_pass.set_pipeline(ctx.pipelines.compute.get(pipelines.cull)?);
        compute_pass.set_bind_group(0, self.bind_groups.get_bind_group()?, None)?;
        let workgroups = decal_count.div_ceil(64);
        compute_pass.dispatch_workgroups(workgroups, Some(1), Some(1));
        compute_pass.end();
        Ok(())
    }
}
