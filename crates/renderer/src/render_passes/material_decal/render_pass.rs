//! Material decal render pass execution — Cluster 6.4, plan §16.4.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    decals::Decals,
    error::Result,
    render::RenderContext,
    render_passes::{
        material_decal::{
            bind_group::MaterialDecalBindGroups, pipeline::MaterialDecalPipelines,
        },
        RenderPassInitContext,
    },
};

/// Material decal pass bind groups and pipelines.
pub struct MaterialDecalRenderPass {
    pub bind_groups: MaterialDecalBindGroups,
    pub pipelines: MaterialDecalPipelines,
}

impl MaterialDecalRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = MaterialDecalBindGroups::new(ctx).await?;
        let pipelines = MaterialDecalPipelines::new(ctx, &bind_groups).await?;
        Ok(Self {
            bind_groups,
            pipelines,
        })
    }

    /// Dispatches the decal compute. Skipped when:
    /// - No decals are active (cheap CPU branch — most frames).
    /// - MSAA is enabled. The transparent texture can't be
    ///   storage-bound when multisampled, so the v1 decal pass has no
    ///   output target. The §16.4 follow-up adds a dedicated
    ///   `decal_color_tex` storage texture + a composite step that
    ///   lifts this restriction.
    pub fn render(&self, ctx: &RenderContext, decals: &Decals) -> Result<()> {
        if decals.is_empty() {
            return Ok(());
        }
        if ctx.anti_aliasing.msaa_sample_count.is_some() {
            return Ok(());
        }

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Decal Pass")).into(),
        ));

        compute_pass.set_pipeline(
            ctx.pipelines
                .compute
                .get(self.pipelines.singlesampled_pipeline_key)?,
        );
        compute_pass.set_bind_group(0, self.bind_groups.get_main()?, None)?;
        compute_pass.set_bind_group(1, self.bind_groups.get_texture_pool()?, None)?;

        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);
        compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));
        compute_pass.end();
        Ok(())
    }
}
