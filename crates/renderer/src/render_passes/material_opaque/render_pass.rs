//! Opaque material render pass execution.

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        material_opaque::{
            bind_group::MaterialOpaqueBindGroups, pipeline::MaterialOpaquePipelines,
        },
        RenderPassInitContext,
    },
    renderable::Renderable,
};
use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;
use slotmap::SecondaryMap;

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
    pub fn render(&self, ctx: &RenderContext, renderables: Vec<Renderable>) -> Result<()> {
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Opaque Pass")).into(),
        ));

        let (main_bind_group, lights_bind_group, texture_bind_group, shadows_bind_group) =
            self.bind_groups.get_bind_groups()?;

        compute_pass.set_bind_group(0u32, main_bind_group, None)?;
        compute_pass.set_bind_group(1u32, lights_bind_group, None)?;
        compute_pass.set_bind_group(2u32, texture_bind_group, None)?;
        compute_pass.set_bind_group(3u32, shadows_bind_group, None)?;

        let workgroup_size = (
            ctx.render_texture_views.width.div_ceil(8),
            ctx.render_texture_views.height.div_ceil(8),
        );

        if renderables.is_empty() {
            if let Some(key) = self
                .pipelines
                .get_empty_compute_pipeline_key(ctx.anti_aliasing)
            {
                compute_pass.set_pipeline(ctx.pipelines.compute.get(key)?);
                compute_pass.dispatch_workgroups(workgroup_size.0, Some(workgroup_size.1), Some(1));
            }
        } else {
            let mut seen_pipeline_keys = SecondaryMap::new();
            for renderable in renderables {
                if let Some(compute_pipeline_key) =
                    renderable.material_opaque_compute_pipeline_key()
                {
                    // only need to dispatch once per pipeline, not per renderable
                    if !seen_pipeline_keys.contains_key(compute_pipeline_key) {
                        seen_pipeline_keys.insert(compute_pipeline_key, ());

                        compute_pass.set_pipeline(ctx.pipelines.compute.get(compute_pipeline_key)?);
                        compute_pass.dispatch_workgroups(
                            workgroup_size.0,
                            Some(workgroup_size.1),
                            Some(1),
                        );
                    }
                }
            }
        }

        compute_pass.end();

        Ok(())
    }
}
