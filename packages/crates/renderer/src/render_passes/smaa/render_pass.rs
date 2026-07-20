//! SMAA pre-pass execution: two dispatches (edges → weights) in one compute
//! pass, run before the bloom pyramid / effects passes each frame while SMAA
//! is enabled. The third SMAA stage (neighborhood blending) lives in the
//! effects shader behind its `smaa_anti_alias` flag, consuming the weights
//! texture this pass produces.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    render::RenderContext,
    render_passes::{
        smaa::{bind_group::SmaaBindGroups, pipeline::SmaaPipelines, texture::SmaaTextures},
        RenderPassInitContext,
    },
};

pub struct SmaaRenderPass {
    pub bind_groups: SmaaBindGroups,
    pub pipelines: SmaaPipelines,
    pub textures: SmaaTextures,
}

impl SmaaRenderPass {
    pub async fn new(
        ctx: &mut RenderPassInitContext<'_>,
        view_width: u32,
        view_height: u32,
    ) -> Result<Self> {
        let bind_groups = SmaaBindGroups::new(ctx)?;
        let pipelines = SmaaPipelines::new(ctx, &bind_groups).await?;
        let textures = SmaaTextures::new(ctx.gpu, view_width.max(1), view_height.max(1))?;
        Ok(Self {
            bind_groups,
            pipelines,
            textures,
        })
    }

    /// Recreate the intermediate textures if the viewport changed. Returns
    /// `true` when textures were rebuilt (callers mark the bind-group ledger).
    pub fn ensure_size(
        &mut self,
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        view_width: u32,
        view_height: u32,
    ) -> Result<bool> {
        let (w, h) = (view_width.max(1), view_height.max(1));
        if self.textures.width == w && self.textures.height == h {
            return Ok(false);
        }
        self.textures.resize(gpu, w, h)?;
        Ok(true)
    }

    /// Dispatch edges + weights. WebGPU inserts the storage-write→sample
    /// barrier between the two dispatches.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let workgroups = (
            self.textures.width.div_ceil(8),
            self.textures.height.div_ceil(8),
        );

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("SMAA Pre-Pass"))
                .with_timestamp_writes_opt(
                    ctx.gpu_timestamps
                        .and_then(|t| t.writes_for_compute("Smaa")),
                )
                .into(),
        ));

        compute_pass.set_bind_group(0, self.bind_groups.edges()?, None)?;
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.edges)?);
        compute_pass.dispatch_workgroups(workgroups.0, Some(workgroups.1), Some(1));

        compute_pass.set_bind_group(0, self.bind_groups.weights()?, None)?;
        compute_pass.set_pipeline(ctx.pipelines.compute.get(self.pipelines.weights)?);
        compute_pass.dispatch_workgroups(workgroups.0, Some(workgroups.1), Some(1));

        compute_pass.end();
        Ok(())
    }
}
