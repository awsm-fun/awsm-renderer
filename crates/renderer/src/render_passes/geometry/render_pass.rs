//! Geometry render pass execution.

use std::sync::LazyLock;

use awsm_renderer_core::command::{
    color::Color,
    render_pass::{ColorAttachment, DepthStencilAttachment, RenderPassDescriptor},
    LoadOp, StoreOp,
};

use crate::{
    debug::{debug_unique_string, DEBUG_ID_RENDERABLE},
    error::Result,
    render::RenderContext,
    render_passes::{
        geometry::{bind_group::GeometryBindGroups, pipeline::GeometryPipelines},
        RenderPassInitContext,
    },
    renderable::Renderable,
};

static VISIBILITY_CLEAR_COLOR: LazyLock<Color> = LazyLock::new(|| {
    let max = f32::MAX.into();
    Color {
        r: max,
        g: max,
        b: max,
        a: max,
    }
});

/// Geometry pass bind groups and pipelines.
pub struct GeometryRenderPass {
    pub bind_groups: GeometryBindGroups,
    pub pipelines: GeometryPipelines,
}

impl GeometryRenderPass {
    /// Creates the geometry render pass resources.
    ///
    /// Per the lazy-pool architecture, only the active MSAA branch is
    /// compiled at construction time. The inactive branch is filled
    /// on the first `set_anti_aliasing` flip.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let multisampled_geometry = ctx.anti_aliasing.has_msaa_checked()?;
        let bind_groups = GeometryBindGroups::new(ctx).await?;
        let pipelines = GeometryPipelines::new(ctx, &bind_groups, multisampled_geometry).await?;

        Ok(Self {
            bind_groups,
            pipelines,
        })
    }

    /// Executes the geometry render pass.
    pub fn render(
        &self,
        ctx: &RenderContext,
        renderables: &[Renderable],
        is_hud: bool,
    ) -> Result<()> {
        let color_attachments = if is_hud {
            vec![
                ColorAttachment::new(
                    &ctx.render_texture_views.visibility_data,
                    LoadOp::Load,
                    StoreOp::Store,
                )
                .with_clear_color(&VISIBILITY_CLEAR_COLOR),
                ColorAttachment::new(
                    &ctx.render_texture_views.barycentric,
                    LoadOp::Load,
                    StoreOp::Store,
                ),
                ColorAttachment::new(
                    &ctx.render_texture_views.normal_tangent,
                    LoadOp::Load,
                    StoreOp::Store,
                ),
                ColorAttachment::new(
                    &ctx.render_texture_views.barycentric_derivatives,
                    LoadOp::Load,
                    StoreOp::Store,
                ),
            ]
        } else {
            vec![
                ColorAttachment::new(
                    &ctx.render_texture_views.visibility_data,
                    LoadOp::Clear,
                    StoreOp::Store,
                )
                .with_clear_color(&VISIBILITY_CLEAR_COLOR),
                ColorAttachment::new(
                    &ctx.render_texture_views.barycentric,
                    LoadOp::Clear,
                    StoreOp::Store,
                ),
                ColorAttachment::new(
                    &ctx.render_texture_views.normal_tangent,
                    LoadOp::Clear,
                    StoreOp::Store,
                ),
                ColorAttachment::new(
                    &ctx.render_texture_views.barycentric_derivatives,
                    LoadOp::Clear,
                    StoreOp::Store,
                ),
            ]
        };

        // T2.6: `hud_depth` is Optional — built only after the first
        // HUD-flagged mesh registers. The caller in
        // `AwsmRenderer::render` gates the HUD geometry pass on
        // `!renderables.hud.is_empty()` (T1.10), and any non-empty
        // HUD set implies `Meshes::has_seen_hud == true` which in
        // turn allocates the texture. By the time `is_hud == true`
        // here, `hud_depth` is therefore guaranteed `Some`.
        let depth_view = if is_hud {
            ctx.render_texture_views.hud_depth.as_ref().expect(
                "hud_depth view absent during HUD geometry pass — invariant violated: \
                 a HUD renderable must flip Meshes::has_seen_hud before any HUD pass call",
            )
        } else {
            &ctx.render_texture_views.depth
        };
        let depth_stencil_attachment = DepthStencilAttachment::new(depth_view)
            .with_depth_load_op(LoadOp::Clear)
            .with_depth_store_op(StoreOp::Store)
            .with_depth_clear_value(1.0);

        let render_pass = ctx.command_encoder.begin_render_pass(
            &RenderPassDescriptor {
                label: Some("Geometry Render Pass"),
                color_attachments,
                depth_stencil_attachment: Some(depth_stencil_attachment),
                ..Default::default()
            }
            .into(),
        )?;

        render_pass.set_bind_group(0, self.bind_groups.camera.get_bind_group()?, None)?;

        render_pass.set_bind_group(1, self.bind_groups.transforms.get_bind_group()?, None)?;

        render_pass.set_bind_group(3, self.bind_groups.animation.get_bind_group()?, None)?;

        let mut last_render_pipeline_key = None;
        for renderable in renderables {
            match renderable.geometry_render_pipeline_key() {
                Some(render_pipeline_key) => {
                    if last_render_pipeline_key != Some(render_pipeline_key) {
                        render_pass.set_pipeline(ctx.pipelines.render.get(render_pipeline_key)?);
                        last_render_pipeline_key = Some(render_pipeline_key);
                    }

                    renderable.push_geometry_pass_commands(ctx, &render_pass, &self.bind_groups)?;
                }
                None => {
                    debug_unique_string(
                        DEBUG_ID_RENDERABLE,
                        &format!("missing pipeline for mesh {:?}", renderable.key),
                        || {
                            tracing::warn!(
                                "Skipping renderable in Geometry Pass due to missing pipeline: {:?}",
                                renderable
                            )
                        },
                    );
                }
            }
        }

        render_pass.end();

        Ok(())
    }
}
