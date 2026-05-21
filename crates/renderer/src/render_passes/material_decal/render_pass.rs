//! Material decal render pass execution.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    decals::Decals,
    error::Result,
    render::RenderContext,
    render_passes::{
        material_decal::{
            bind_group::MaterialDecalBindGroups, classify::render_pass::DecalClassifyRenderPass,
            composite::MaterialDecalComposite, pipeline::MaterialDecalPipelines,
        },
        RenderPassInitContext,
    },
};

/// Material decal pass bind groups, compute pipelines, the
/// downstream composite pass, and the upstream per-tile classify pass.
pub struct MaterialDecalRenderPass {
    pub bind_groups: MaterialDecalBindGroups,
    pub pipelines: MaterialDecalPipelines,
    pub composite: MaterialDecalComposite,
    pub classify_pass: DecalClassifyRenderPass,
}

impl MaterialDecalRenderPass {
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = MaterialDecalBindGroups::new(ctx).await?;
        let pipelines = MaterialDecalPipelines::new(ctx, &bind_groups).await?;
        let composite = MaterialDecalComposite::new(ctx).await?;
        let classify_pass = DecalClassifyRenderPass::new(ctx).await?;
        Ok(Self {
            bind_groups,
            pipelines,
            composite,
            classify_pass,
        })
    }

    /// Dispatches: classify → compute → composite. Skipped when no
    /// decals are active.
    pub fn render(&self, ctx: &RenderContext, decals: &Decals) -> Result<()> {
        if decals.is_empty() {
            return Ok(());
        }

        // Tile-bucket classify must run before the shading compute so
        // per-pixel iteration reads from a fresh per-tile decal list.
        self.classify_pass.render(ctx, decals.len() as u32)?;

        let pipeline_key = if ctx.anti_aliasing.msaa_sample_count.is_some() {
            self.pipelines.multisampled_pipeline_key
        } else {
            self.pipelines.singlesampled_pipeline_key
        };

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Decal Pass")).into(),
        ));

        compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
        compute_pass.set_bind_group(0, self.bind_groups.get_main()?, None)?;
        compute_pass.set_bind_group(1, self.bind_groups.get_texture_pool()?, None)?;

        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);
        compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));
        compute_pass.end();

        // Composite pass — blit decal_color onto transparent. Cheap
        // fullscreen-tri with per-fragment discard; per-frame cost is
        // negligible vs the compute that just ran.
        self.composite.render(ctx)?;

        Ok(())
    }
}
