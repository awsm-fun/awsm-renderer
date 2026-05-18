//! Transparent material render pass execution.

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        material_transparent::{
            bind_group::MaterialTransparentBindGroups, pipeline::MaterialTransparentPipelines,
        },
        RenderPassInitContext,
    },
    renderable::Renderable,
};

/// Transparent material pass bind groups and pipelines.
pub struct MaterialTransparentRenderPass {
    pub bind_groups: MaterialTransparentBindGroups,
    pub pipelines: MaterialTransparentPipelines,
}

impl MaterialTransparentRenderPass {
    /// Creates the transparent material render pass resources.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = MaterialTransparentBindGroups::new(ctx).await?;
        let pipelines = MaterialTransparentPipelines::new(ctx, &bind_groups).await?;

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
        self.pipelines = MaterialTransparentPipelines::new(ctx, &self.bind_groups).await?;

        Ok(())
    }

    /// Executes the transparent material pass.
    pub fn render(
        &self,
        ctx: &RenderContext,
        renderables: Vec<Renderable>,
        is_hud: bool,
    ) -> Result<()> {
        let render_pass = if is_hud {
            ctx.begin_hud_transparent_pass(Some("Material Transparent Pass (HUD)"))?
        } else {
            ctx.begin_world_transparent_pass(Some("Material Transparent Pass"))?
        };

        let (main_bind_group, mesh_material_bind_group, lights_bind_group, texture_bind_group) =
            self.bind_groups.get_bind_groups()?;

        // set later with dynamic offsets
        render_pass.set_bind_group(0u32, main_bind_group, None)?;
        render_pass.set_bind_group(1u32, lights_bind_group, None)?;
        render_pass.set_bind_group(2u32, texture_bind_group, None)?;
        // Phase 9 will add a shadow bind group here once `maxBindGroups=4`
        // is respected by collapsing an existing group.

        let mut last_render_pipeline_key = None;
        for renderable in renderables {
            if let Some(render_pipeline_key) =
                renderable.material_transparent_render_pipeline_key(ctx)
            {
                if last_render_pipeline_key != Some(render_pipeline_key) {
                    render_pass.set_pipeline(ctx.pipelines.render.get(render_pipeline_key)?);
                    last_render_pipeline_key = Some(render_pipeline_key);
                }

                renderable.push_material_transparent_pass_commands(
                    ctx,
                    &render_pass,
                    mesh_material_bind_group,
                )?;
            }
        }

        render_pass.end();

        Ok(())
    }
}
