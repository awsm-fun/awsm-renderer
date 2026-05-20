//! Material classify render pass execution — Cluster 6.1, plan
//! §16.3.B. Produces per-`shader_id` tile buckets + indirect-dispatch
//! args consumed by the opaque material pipelines.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        material_classify::{
            bind_group::MaterialClassifyBindGroups, pipeline::MaterialClassifyPipelines,
        },
        RenderPassInitContext,
    },
};

/// Material classify pass bind groups and pipelines.
pub struct MaterialClassifyRenderPass {
    pub bind_groups: MaterialClassifyBindGroups,
    pub pipelines: MaterialClassifyPipelines,
}

impl MaterialClassifyRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = MaterialClassifyBindGroups::new(ctx).await?;
        let pipelines = MaterialClassifyPipelines::new(ctx, &bind_groups).await?;
        Ok(Self {
            bind_groups,
            pipelines,
        })
    }

    /// Dispatches the classify shader: one workgroup per 8×8 tile of
    /// the visibility buffer. Per-workgroup atomic-or builds a bucket
    /// mask, then thread 0 atomically appends the tile to each
    /// bucket bit it touched.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Classify Pass")).into(),
        ));

        let pipeline_key = if ctx.anti_aliasing.msaa_sample_count.is_some() {
            self.pipelines.multisampled_pipeline_key
        } else {
            self.pipelines.singlesampled_pipeline_key
        };

        compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
        compute_pass.set_bind_group(0, self.bind_groups.get_bind_group()?, None)?;

        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);
        compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));

        compute_pass.end();
        Ok(())
    }
}
