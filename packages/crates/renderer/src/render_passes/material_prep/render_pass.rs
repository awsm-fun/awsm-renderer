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

use std::borrow::Cow;

use awsm_renderer_core::bind_groups::{BindGroupDescriptor, BindGroupEntry, BindGroupResource};
use awsm_renderer_core::buffers::BufferBinding;
use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;

use crate::{
    error::Result,
    pipeline_layouts::PipelineLayoutCacheKey,
    pipelines::compute_pipeline::{ComputePipelineCacheKey, ComputePipelineKey},
    render::RenderContext,
    render_passes::{
        material_opaque::edge_buffers::MaterialEdgeBuffers,
        material_prep::{
            bind_group::MaterialPrepBindGroups, buffers::EdgeShadowBuffer,
            shader::cache_key::ShaderCacheKeyMaterialPrep,
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
    /// Stage 5b-shadow: `cs_prep_edge` pipeline (MSAA only — `None` otherwise).
    /// Indirect-dispatched over `edge_count`, filling `edge_shadow` so the MSAA
    /// `cs_edge` reads per-edge-sample shadow visibility instead of inline
    /// sampling.
    pub edge_pipeline_key: Option<ComputePipelineKey>,
    /// Stage 5b-shadow: the compact per-edge-sample shadow texture cs_prep_edge
    /// writes + cs_edge reads. `None` when not MSAA.
    pub edge_shadow: Option<EdgeShadowBuffer>,
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

        // Stage 5b-shadow: the cs_prep_edge pipeline + compact edge-shadow
        // texture. Built eagerly (not gated on build-time MSAA) so an
        // `set_anti_aliasing(off → on)` flip finds them ready — the cs_prep_edge
        // pipeline shares the always-built multisampled main layout, and the
        // texture costs ~8 MB. `render_edge` only dispatches when the edge
        // buffers exist (MSAA on), so this is inert otherwise. The texture is
        // sized from the resolved edge budget; layers = ceil(K/4).
        let edge_pipeline_key = Some(build_edge_pipeline(ctx, &bind_groups).await?);
        let edge_shadow = Some(EdgeShadowBuffer::new(
            ctx.gpu,
            ctx.max_edge_budget,
            ctx.prep_config.shadow_visibility_layers(),
        )?);

        Ok(Self {
            bind_groups,
            multisampled_pipeline_key,
            singlesampled_pipeline_key,
            edge_pipeline_key,
            edge_shadow,
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
        let lights_bind_group = self.bind_groups.get_lights_bind_group()?;
        let shadows_bind_group = self.bind_groups.get_shadows_bind_group()?;

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Prep Pass")).into(),
        ));
        compute_pass.set_pipeline(pipeline);
        compute_pass.set_bind_group(0, bind_group, None)?;
        compute_pass.set_bind_group(1, lights_bind_group, None)?;
        compute_pass.set_bind_group(2, shadows_bind_group, None)?;

        let workgroups_x = ctx.render_texture_views.width.div_ceil(8);
        let workgroups_y = ctx.render_texture_views.height.div_ceil(8);
        compute_pass.dispatch_workgroups(workgroups_x, Some(workgroups_y), Some(1));

        compute_pass.end();
        Ok(())
    }

    /// Stage 5b-shadow: resize the compact edge-shadow texture to a new edge
    /// budget (mirrors `MaterialEdgeBuffers::set_max_edge_budget`). No-op when the
    /// budget is unchanged or this prep pass has no edge texture (non-MSAA).
    /// Caller marks the dependent bind groups (opaque main binding 27) dirty.
    pub fn set_max_edge_budget(
        &mut self,
        gpu: &awsm_renderer_core::renderer::AwsmRendererWebGpu,
        new_budget: u32,
    ) -> Result<bool> {
        let Some(existing) = self.edge_shadow.as_ref() else {
            return Ok(false);
        };
        if existing.max_edge_budget == new_budget.max(1) {
            return Ok(false);
        }
        let layers = existing.layers;
        self.edge_shadow = Some(EdgeShadowBuffer::new(gpu, new_budget, layers)?);
        Ok(true)
    }

    /// Stage 5b-shadow: dispatch `cs_prep_edge` — fills the compact per-edge-
    /// sample shadow texture so the MSAA `cs_edge` can read it instead of inline
    /// sampling shadow maps. Indirect over `edge_count` (reuses the
    /// `final_blend_args` DispatchIndirectArgs cell, already sized for all
    /// edges). Inserted between `cs_prep` and the opaque pass; only effective
    /// under MSAA (the pipeline + texture are `None` otherwise). No-op when the
    /// edge buffers / layout uniform aren't allocated (non-MSAA).
    pub fn render_edge(&self, ctx: &RenderContext) -> Result<()> {
        let (edge_pipeline_key, edge_shadow) =
            match (self.edge_pipeline_key, self.edge_shadow.as_ref()) {
                (Some(k), Some(b)) => (k, b),
                _ => return Ok(()),
            };
        let edge_bgl_key = match self.bind_groups.edge_bind_group_layout_key {
            Some(k) => k,
            None => return Ok(()),
        };
        let (edge_buffers, edge_layout_uniform) =
            match (ctx.material_edge_buffers, ctx.material_edge_layout_uniform) {
                (Some(b), Some(u)) => (b, u),
                _ => return Ok(()),
            };

        let pipeline = ctx.pipelines.compute.get(edge_pipeline_key)?;
        let bind_group = self.bind_groups.get_bind_group()?;
        let lights_bind_group = self.bind_groups.get_lights_bind_group()?;
        let shadows_bind_group = self.bind_groups.get_shadows_bind_group()?;

        // group(3) built fresh each frame (cheap; mirrors the opaque edge-resolve
        // pass): edge_data (RO) + edge_layout + edge_shadow_out (storage write).
        let entries = vec![
            BindGroupEntry::new(
                0,
                BindGroupResource::Buffer(BufferBinding::new(&edge_buffers.data_buffer)),
            ),
            BindGroupEntry::new(
                1,
                BindGroupResource::Buffer(BufferBinding::new(edge_layout_uniform)),
            ),
            BindGroupEntry::new(
                2,
                BindGroupResource::TextureView(Cow::Borrowed(&edge_shadow.storage_view)),
            ),
        ];
        let descriptor = BindGroupDescriptor::new(
            ctx.bind_group_layouts.get(edge_bgl_key)?,
            Some("Material Prep Edge - Group 3"),
            entries,
        );
        let edge_bind_group = ctx.gpu.create_bind_group(&descriptor.into());

        let compute_pass = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Material Prep Edge Pass")).into(),
        ));
        compute_pass.set_pipeline(pipeline);
        compute_pass.set_bind_group(0, bind_group, None)?;
        compute_pass.set_bind_group(1, lights_bind_group, None)?;
        compute_pass.set_bind_group(2, shadows_bind_group, None)?;
        compute_pass.set_bind_group(3, &edge_bind_group, None)?;
        // Indirect over edge_count via the final_blend_args cell (workgroup_size
        // 64; the cell's workgroup_count_x = ceil(edge_count / 64), set by
        // classify — already sized for all edges).
        compute_pass.dispatch_workgroups_indirect_with_u32(
            &edge_buffers.args_buffer,
            MaterialEdgeBuffers::final_blend_args_offset(),
        );
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
        PipelineLayoutCacheKey::new(vec![
            bgl_key,
            bind_groups.lights_bind_group_layout_key,
            bind_groups.shadows_bind_group_layout_key,
        ]),
    )?;
    let shader_key = ctx
        .shaders
        .get_key(
            ctx.gpu,
            ShaderCacheKeyMaterialPrep {
                msaa_sample_count: if multisampled_geometry { Some(4) } else { None },
                max_shadow_casters: ctx.prep_config.clamped_k(),
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

/// Stage 5b-shadow: builds the `cs_prep_edge` pipeline (MSAA only). Shares the
/// MSAA prep shader module (same cache key as the multisampled `cs_prep`); its
/// pipeline layout adds group(3) = the edge layout (edge_data + edge_layout +
/// edge_shadow_out) on top of the multisampled main + lights + shadows groups.
async fn build_edge_pipeline(
    ctx: &mut RenderPassInitContext<'_>,
    bind_groups: &MaterialPrepBindGroups,
) -> Result<ComputePipelineKey> {
    let edge_bgl_key = bind_groups
        .edge_bind_group_layout_key
        .expect("edge bind group layout must exist under MSAA");
    let pipeline_layout_key = ctx.pipeline_layouts.get_key(
        ctx.gpu,
        ctx.bind_group_layouts,
        PipelineLayoutCacheKey::new(vec![
            bind_groups.multisampled_bind_group_layout_key,
            bind_groups.lights_bind_group_layout_key,
            bind_groups.shadows_bind_group_layout_key,
            edge_bgl_key,
        ]),
    )?;
    let shader_key = ctx
        .shaders
        .get_key(
            ctx.gpu,
            ShaderCacheKeyMaterialPrep {
                msaa_sample_count: Some(4),
                max_shadow_casters: ctx.prep_config.clamped_k(),
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
                .with_entry_point("cs_prep_edge"),
        )
        .await?;
    Ok(pipeline_key)
}
