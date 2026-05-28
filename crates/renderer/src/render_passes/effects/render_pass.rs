//! Effects render pass execution.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        effects::{
            bind_group::EffectsBindGroups,
            pipeline::{EffectsPipelines, BLOOM_BLUR_PASSES},
            shader::cache_key::BloomPhase,
        },
        RenderPassInitContext,
    },
};

/// Effects pass bind groups and pipelines.
pub struct EffectsRenderPass {
    pub bind_groups: EffectsBindGroups,
    pub pipelines: EffectsPipelines,
}

impl EffectsRenderPass {
    /// Creates the effects render pass resources.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = EffectsBindGroups::new(ctx).await?;
        let pipelines = EffectsPipelines::new(ctx, &bind_groups).await?;

        Ok(Self {
            bind_groups,
            pipelines,
        })
    }

    /// Executes the effects pass.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        // Skip the entire pass when nothing is configured to consume it.
        // The fallback `BloomPhase::None` dispatch existed historically
        // to host SMAA/DoF wiring, but when bloom + dof + smaa are all
        // off it's a viewport-wide compute dispatch (workgroup grid
        // `(W/8, H/8)`) that does no observable work — and on a TBR
        // mobile GPU each begin_compute_pass forces a tile flush. The
        // Display pass continues to do tonemapping/exposure on its own,
        // so dropping this pass changes nothing visible. SMAA flips back
        // on independently below.
        if !ctx.post_processing.bloom && !ctx.post_processing.dof && !ctx.anti_aliasing.smaa {
            return Ok(());
        }

        let workgroup_size = (
            ctx.render_texture_views.width.div_ceil(8),
            ctx.render_texture_views.height.div_ceil(8),
        );

        if ctx.post_processing.bloom {
            // Multi-pass bloom
            // Pass 0: Extract bright pixels (ping_pong=false, writes to effects_tex)
            self.dispatch_pass(ctx, BloomPhase::Extract, false, workgroup_size)?;

            // Passes 1..=BLOOM_BLUR_PASSES: Blur passes (alternating ping_pong)
            for i in 0..BLOOM_BLUR_PASSES {
                let ping_pong = (i + 1) % 2 == 1; // Pass 1 is ping_pong=true, pass 2 is false, etc.
                self.dispatch_pass(ctx, BloomPhase::Blur, ping_pong, workgroup_size)?;
            }

            // Final pass: Blend with original
            // ping_pong must match what we calculated in pipeline.rs
            let blend_ping_pong = (1 + BLOOM_BLUR_PASSES) % 2 == 1;
            self.dispatch_pass(ctx, BloomPhase::Blend, blend_ping_pong, workgroup_size)?;
        } else {
            // Single pass for other effects only (SMAA, DoF)
            self.dispatch_pass(ctx, BloomPhase::None, false, workgroup_size)?;
        }

        Ok(())
    }

    fn dispatch_pass(
        &self,
        ctx: &RenderContext,
        phase: BloomPhase,
        ping_pong: bool,
        workgroup_size: (u32, u32),
    ) -> Result<()> {
        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Effects Pass")).into(),
        ));

        compute_pass.set_bind_group(0, self.bind_groups.get_bind_group(ping_pong)?, None)?;

        if let Some(pipeline_key) = self.pipelines.get_bloom_pipeline(phase, ping_pong) {
            compute_pass.set_pipeline(ctx.pipelines.compute.get(pipeline_key)?);
            compute_pass.dispatch_workgroups(workgroup_size.0, Some(workgroup_size.1), Some(1));
        }

        compute_pass.end();

        Ok(())
    }
}
