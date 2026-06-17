//! Material prep render pass execution (Plan B,
//! docs/plans/deferred-shared-prep-pass.md).
//!
//! A static compute pass (mirrors [`crate::render_passes::light_culling`]): runs
//! once per pixel over the visibility buffer, after classify and before
//! per-material shading, materializing the material-INDEPENDENT geometry-pool
//! attributes (UV0 + vertex color) into the prep output storage textures. There
//! are exactly two pipeline variants — multisampled-geometry on/off — both built
//! up-front so an MSAA change needs only a bind-group rebuild, not a recompile.
//!
//! Only constructed (and dispatched) when `PrepPassConfig.enabled`. When the
//! flag is off the pass is `None`, so the legacy path is byte-identical.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey},
    pipeline_layouts::PipelineLayoutCacheKey,
    render::RenderContext,
    render_passes::{
        material_prep::{
            bind_group::MaterialPrepBindGroups, shader::cache_key::ShaderCacheKeyMaterialPrep,
        },
        RenderPassInitContext,
    },
};

/// Material prep pass bind groups + the two compiled (MSAA on/off) pipelines.
pub struct MaterialPrepRenderPass {
    pub bind_groups: MaterialPrepBindGroups,
    /// Compiled `cs_prep` pipeline for the multisampled-geometry variant.
    pub multisampled_pipeline_key: ComputePipelineKey,
    /// Compiled `cs_prep` pipeline for the single-sample variant.
    pub singlesampled_pipeline_key: ComputePipelineKey,
}

impl MaterialPrepRenderPass {
    /// Creates the prep render pass resources. Eager compile of both MSAA
    /// variants — matches the static-compute-pass convention
    /// ([`crate::render_passes::light_culling`]). Only called when
    /// `PrepPassConfig.enabled`.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = MaterialPrepBindGroups::new(ctx).await?;
        let multisampled_pipeline_key = build_pipeline(ctx, &bind_groups, true).await?;
        let singlesampled_pipeline_key = build_pipeline(ctx, &bind_groups, false).await?;
        Ok(Self {
            bind_groups,
            multisampled_pipeline_key,
            singlesampled_pipeline_key,
        })
    }

    /// Dispatches the prep shader: one workgroup per 8×8 tile of the
    /// visibility buffer. Picks the pipeline variant matching the live MSAA
    /// state.
    pub fn render(&self, ctx: &RenderContext) -> Result<()> {
        let pipeline_key = if ctx.anti_aliasing.msaa_sample_count.is_some() {
            self.multisampled_pipeline_key
        } else {
            self.singlesampled_pipeline_key
        };
        let pipeline = ctx.pipelines.compute.get(pipeline_key)?;
        let bind_group = self.bind_groups.get_bind_group()?;

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Prep Pass")).into(),
        ));
        compute_pass.set_pipeline(pipeline);
        compute_pass.set_bind_group(0, bind_group, None)?;

        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);
        compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));

        compute_pass.end();
        Ok(())
    }
}

/// Builds the `cs_prep` pipeline for one MSAA-geometry variant.
async fn build_pipeline(
    ctx: &mut RenderPassInitContext<'_>,
    bind_groups: &MaterialPrepBindGroups,
    multisampled_geometry: bool,
) -> Result<ComputePipelineKey> {
    let bgl_key = if multisampled_geometry {
        bind_groups.multisampled_bind_group_layout_key
    } else {
        bind_groups.singlesampled_bind_group_layout_key
    };
    let pipeline_layout_key = ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![bgl_key]),
    )?;
    let shader_key = ctx
        .shaders
        .get_key(
            ctx.gpu,
            ShaderCacheKeyMaterialPrep {
                msaa_sample_count: if multisampled_geometry { Some(4) } else { None },
            },
        )
        .await?;
    let pipeline_key = ctx
        .pipelines
        .compute
        .get_key(
            ctx.gpu,
            ctx.shaders,
            ctx.pipeline_layouts,
            ComputePipelineCacheKey::new(shader_key, pipeline_layout_key)
                .with_entry_point("cs_prep"),
        )
        .await?;
    Ok(pipeline_key)
}
