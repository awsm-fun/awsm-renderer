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
        // PR #103 review note: T1.5 originally attempted to early-exit
        // here when bloom/dof/smaa are all off, on the theory that the
        // `BloomPhase::None` dispatch is a no-op. It isn't. The shader
        // unconditionally does `textureLoad(composite_tex)` followed
        // by `textureStore(effects_tex, …)` — i.e. it's the only thing
        // that puts pixels into `effects_tex`, which the display pass
        // then samples at binding 0 (see `display/bind_group.rs:99`).
        //
        // Skipping the dispatch left `effects_tex` with stale /
        // driver-defined contents and the display pass showed garbage
        // (or last-known-effects-frame contents) on every frame where
        // no effects were enabled. Reinstating the dispatch is the
        // safe path. The bandwidth cost is small (~5 MB at 400×800)
        // and the only theoretical win was the compute-pass
        // open/close overhead (~30 µs on mobile), recovering which
        // would require plumbing post-processing state into the
        // display bind-group recreation flow so display could sample
        // `composite` directly — disproportionate complexity for the
        // saving. Re-evaluate in a follow-up if profiling justifies.
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
