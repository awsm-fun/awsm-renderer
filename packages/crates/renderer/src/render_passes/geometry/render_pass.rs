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
        geometry::{
            bind_group::GeometryBindGroups, custom_vertex_pipeline::GeometryCustomVertexPipelines,
            masked_bind_group::GeometryMaskedBindGroup, masked_pipeline::GeometryMaskedPipelines,
            pipeline::GeometryPipelines,
        },
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
    /// Augmented group-0 bind group bound for the masked (alpha-tested)
    /// variant draws. See [`GeometryMaskedBindGroup`].
    pub masked_bind_group: GeometryMaskedBindGroup,
    /// Lazy per-`shader_id` pool of masked (alpha-tested) pipelines.
    /// Populated by the texture-finalize flow (built-in) + the dynamic
    /// scheduler (custom); empty until a masked material needs one.
    pub masked_pipelines: GeometryMaskedPipelines,
    /// Lazy per-`shader_id` pool of custom-vertex pipelines. Populated by the
    /// texture-finalize flow for every registered material whose
    /// `vertex_shader_info_for` is `Some`; empty until a custom-vertex material
    /// needs one. Reuses `masked_bind_group` for group 0 (the custom-vertex
    /// `bind_groups.wgsl` declares the same bindings).
    pub custom_vertex_pipelines: GeometryCustomVertexPipelines,
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
        let masked_bind_group = GeometryMaskedBindGroup::new(ctx).await?;
        let masked_pipelines = GeometryMaskedPipelines::new(ctx, &masked_bind_group, &bind_groups)?;
        let custom_vertex_pipelines =
            GeometryCustomVertexPipelines::new(ctx, &masked_bind_group, &bind_groups)?;

        Ok(Self {
            bind_groups,
            pipelines,
            masked_bind_group,
            masked_pipelines,
            custom_vertex_pipelines,
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

        // Pass 1 — non-masked meshes (group 0 = camera, bound above). A mesh
        // with a compiled masked OR custom-vertex variant is skipped here and
        // drawn in pass 2 / pass 3 respectively.
        let mut last_render_pipeline_key = None;
        for renderable in renderables {
            if renderable.geometry_masked_render_pipeline_key().is_some()
                || renderable
                    .geometry_custom_vertex_render_pipeline_key()
                    .is_some()
            {
                continue;
            }
            match renderable.geometry_render_pipeline_key() {
                Some(render_pipeline_key) => {
                    if last_render_pipeline_key != Some(render_pipeline_key) {
                        render_pass.set_pipeline(ctx.pipelines.render.get(render_pipeline_key)?);
                        last_render_pipeline_key = Some(render_pipeline_key);
                    }

                    renderable.push_geometry_pass_commands(
                        ctx,
                        &render_pass,
                        &self.bind_groups,
                        false,
                    )?;
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

        // Pass 2 — masked (alpha-tested) meshes. Rebind group 0 to the
        // augmented masked bind group (camera/frame_globals are at the same
        // slots, so the shared vertex still resolves them; groups 1/2/3 keep
        // the plain geometry layouts and stay valid across the pipeline switch).
        let any_masked = renderables
            .iter()
            .any(|r| r.geometry_masked_render_pipeline_key().is_some());
        if any_masked {
            if let Ok(masked_group0) = self.masked_bind_group.get_bind_group() {
                render_pass.set_bind_group(0, masked_group0, None)?;
                let mut last_masked_key = None;
                for renderable in renderables {
                    let Some(masked_key) = renderable.geometry_masked_render_pipeline_key() else {
                        continue;
                    };
                    if last_masked_key != Some(masked_key) {
                        render_pass.set_pipeline(ctx.pipelines.render.get(masked_key)?);
                        last_masked_key = Some(masked_key);
                    }
                    renderable.push_geometry_pass_commands(
                        ctx,
                        &render_pass,
                        &self.bind_groups,
                        true,
                    )?;
                }
            }
        }

        // Pass 3 — custom-vertex meshes. Same group-0 rebind as pass 2 (the
        // custom-vertex pipeline reuses the masked/augmented group-0 layout, so
        // the hook's `material_data_load` resolves the `materials` buffer +
        // texture pool); select the per-material custom-vertex pipeline and bind
        // the shared zero uv0 buffer at the uv0 slot. A mesh drawn here was
        // skipped in pass 1 (above). When its variant hasn't compiled yet the
        // key is `None`, so the mesh stays in pass 1 and renders un-displaced —
        // never dropped.
        let any_custom_vertex = renderables
            .iter()
            .any(|r| r.geometry_custom_vertex_render_pipeline_key().is_some());
        if any_custom_vertex {
            if let Ok(masked_group0) = self.masked_bind_group.get_bind_group() {
                render_pass.set_bind_group(0, masked_group0, None)?;
                let uv0_zero_buffer = self.custom_vertex_pipelines.uv0_zero_buffer();
                let mut last_cv_key = None;
                for renderable in renderables {
                    let Some(cv_key) = renderable.geometry_custom_vertex_render_pipeline_key()
                    else {
                        continue;
                    };
                    if last_cv_key != Some(cv_key) {
                        render_pass.set_pipeline(ctx.pipelines.render.get(cv_key)?);
                        last_cv_key = Some(cv_key);
                    }
                    renderable.push_geometry_custom_vertex_pass_commands(
                        ctx,
                        &render_pass,
                        &self.bind_groups,
                        uv0_zero_buffer,
                    )?;
                }
            }
        }

        render_pass.end();

        Ok(())
    }
}
