//! Cluster-LOD cut compute pass (Phase B, B.2).
//!
//! Built eagerly (like `light_culling` / `material_prep`) and gated by
//! `virtual_geometry`. Holds the cut pipeline + bind-group layout; the per-mesh
//! [`ClusterLodBuffers`] and the bind-group instance are created/recreated when a
//! cluster mesh loads. Inert (no dispatch) until a cluster mesh is present.

use awsm_renderer_core::command::compute_pass::ComputePassDescriptor;
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use glam::{Mat4, Vec3};

use crate::bind_group_layout::BindGroupLayouts;
use crate::cluster_lod::ClusterPage;
use crate::error::Result;
use crate::render::RenderContext;
use crate::render_passes::cluster_lod::{
    bind_group::ClusterCutBindGroups, buffers::ClusterLodBuffers, pipeline::ClusterLodPipelines,
};
use crate::render_passes::RenderPassInitContext;

pub struct ClusterLodRenderPass {
    pub bind_groups: ClusterCutBindGroups,
    pub pipelines: ClusterLodPipelines,
    /// Per-mesh cluster buffers (single cluster mesh for now — a `MeshKey`
    /// registry is the multi-mesh follow-up). `None` until a cluster mesh loads.
    pub buffers: Option<ClusterLodBuffers>,
    /// Number of cluster pages uploaded (the cut dispatch bound).
    pub cluster_count: u32,
}

impl ClusterLodRenderPass {
    /// Builds the bind-group layout + cut compute pipeline. **Creating the
    /// pipeline validates `cluster_cut.wgsl` on-device** (the GPU driver compiles
    /// it here) — the first on-GPU checkpoint for the per-cluster cut.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = ClusterCutBindGroups::new(ctx)?;
        let pipelines = ClusterLodPipelines::new(ctx, &bind_groups).await?;
        Ok(Self {
            bind_groups,
            pipelines,
            buffers: None,
            cluster_count: 0,
        })
    }

    /// Upload a cluster mesh's pages (once, at mesh load): (re)allocate the
    /// buffers to hold `pages`, write them, and rebuild the bind group against
    /// the new buffers. Idempotent per mesh.
    pub fn upload_pages(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        layouts: &BindGroupLayouts,
        pages: &[ClusterPage],
    ) -> Result<()> {
        let count = pages.len() as u32;
        let buffers = match self.buffers.as_mut() {
            Some(b) => {
                b.ensure_capacity(gpu, count)?;
                b
            }
            None => {
                self.buffers = Some(ClusterLodBuffers::with_capacity(gpu, count.max(1))?);
                self.buffers.as_mut().unwrap()
            }
        };
        buffers.write_pages(gpu, pages)?;
        self.cluster_count = count;
        let buffers = self.buffers.as_ref().unwrap();
        self.bind_groups.recreate(gpu, layouts, buffers)?;
        Ok(())
    }

    /// Dispatch the per-cluster cut: write the per-frame params, then run the
    /// `cut` compute over `ceil(cluster_count/64)` workgroups. Writes 0/1 per
    /// cluster into `selected`. No-op without loaded buffers. (Instance world is
    /// identity for now — the per-instance world is the follow-up; the camera +
    /// viewport are live.)
    pub fn dispatch(
        &self,
        ctx: &RenderContext,
        cam_pos: Vec3,
        tan_half_fov_y: f32,
        viewport_h: f32,
        pixel_budget: f32,
    ) -> Result<()> {
        let Some(buffers) = self.buffers.as_ref() else {
            return Ok(());
        };
        if self.cluster_count == 0 {
            return Ok(());
        }
        buffers.write_params(
            ctx.gpu,
            &Mat4::IDENTITY,
            cam_pos,
            tan_half_fov_y,
            viewport_h,
            pixel_budget,
            1.0,
            self.cluster_count,
        )?;
        let cp = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Cluster Cut")).into(),
        ));
        cp.set_pipeline(ctx.pipelines.compute.get(self.pipelines.cut)?);
        cp.set_bind_group(0, self.bind_groups.get_bind_group()?, None)?;
        cp.dispatch_workgroups(
            ClusterLodBuffers::dispatch_groups(self.cluster_count),
            Some(1),
            Some(1),
        );
        cp.end();
        Ok(())
    }
}
