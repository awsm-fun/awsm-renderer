//! Display render pass execution.

use std::cell::Cell;
use std::vec;

use awsm_renderer_core::command::{
    render_pass::{ColorAttachment, RenderPassDescriptor},
    LoadOp, StoreOp,
};

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        display::{bind_group::DisplayBindGroups, pipeline::DisplayPipelines},
        RenderPassInitContext,
    },
};

/// Display pass bind groups and pipelines.
pub struct DisplayRenderPass {
    pub bind_groups: DisplayBindGroups,
    pub pipelines: DisplayPipelines,
    /// Last `exposure_scale` (i.e. `exposure.exp2()`) we uploaded.
    /// `None` until the first frame so the first call always writes;
    /// subsequent frames re-upload only when the value changes.
    /// Exposure rarely changes (camera setting, not animated per
    /// frame), so this gates out the per-frame 4-byte wasm↔JS
    /// `writeBuffer` round trip on the steady-state path. Public so
    /// the renderer-internal struct-literal construction (in
    /// `render_passes.rs::from_resolved`) can initialize it; the
    /// uploader logic still owns the only writes.
    pub last_exposure_scale: Cell<Option<f32>>,
}

impl DisplayRenderPass {
    /// Creates the display render pass resources.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = DisplayBindGroups::new(ctx).await?;
        let pipelines = DisplayPipelines::new(ctx, &bind_groups).await?;

        Ok(Self {
            bind_groups,
            pipelines,
            last_exposure_scale: Cell::new(None),
        })
    }

    /// Executes the display render pass.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        // Upload the per-frame display uniform (currently: exposure scale).
        // exp2(EV) so 0 EV is unity, +1 EV doubles brightness, -1 EV halves.
        // Exposure is a camera setting that rarely changes between frames
        // — skip the `writeBuffer` when the value matches the prior frame.
        let exposure_scale = ctx.post_processing.exposure.exp2();
        let needs_upload = match self.last_exposure_scale.get() {
            Some(prev) => prev.to_bits() != exposure_scale.to_bits(),
            None => true,
        };
        if needs_upload {
            let mut bytes = [0u8; super::bind_group::DISPLAY_UNIFORM_SIZE];
            bytes[0..4].copy_from_slice(&exposure_scale.to_le_bytes());
            ctx.gpu.write_buffer(
                &self.bind_groups.uniform_buffer,
                None,
                &bytes[..],
                None,
                None,
            )?;
            self.last_exposure_scale.set(Some(exposure_scale));
        }

        let render_pass = ctx.command_encoder.begin_render_pass(
            &RenderPassDescriptor {
                label: Some("Display Render Pass"),
                color_attachments: vec![ColorAttachment::new(
                    &ctx.gpu.current_context_texture_view()?,
                    LoadOp::Clear,
                    StoreOp::Store,
                )
                .with_clear_color(ctx.clear_color)],
                ..Default::default()
            }
            .into(),
        )?;

        render_pass.set_bind_group(0, self.bind_groups.get_bind_group()?, None)?;

        if let Some(pipeline_key) = self.pipelines.render_pipeline_key {
            render_pass.set_pipeline(ctx.pipelines.render.get(pipeline_key)?);
            // No vertex buffer needed!
            render_pass.draw(3);
        }

        render_pass.end();

        // TODO!

        Ok(())
    }
}
