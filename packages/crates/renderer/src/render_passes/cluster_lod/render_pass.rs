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
use crate::meshes::MeshKey;
use crate::render::RenderContext;
use crate::render_passes::cluster_lod::{
    bind_group::{ClusterCompactionBindGroups, ClusterCutBindGroups},
    buffers::ClusterLodBuffers,
    pipeline::ClusterLodPipelines,
};
use crate::render_passes::RenderPassInitContext;

pub struct ClusterLodRenderPass {
    pub bind_groups: ClusterCutBindGroups,
    pub compaction_bind_groups: ClusterCompactionBindGroups,
    pub pipelines: ClusterLodPipelines,
    /// Per-mesh cluster buffers (single cluster mesh for now — a `MeshKey`
    /// registry is the multi-mesh follow-up). `None` until a cluster mesh loads.
    pub buffers: Option<ClusterLodBuffers>,
    /// Number of cluster pages uploaded (the cut dispatch bound).
    pub cluster_count: u32,
    /// The cluster render mesh `M` (`add_raw_mesh(cm.positions, cm.indices)`) — an
    /// ordinary mesh whose exploded vertex buffer the compacted indirect stream
    /// draws into (its own draw is hidden). `None` until a cluster mesh loads.
    pub render_mesh: Option<MeshKey>,
}

impl ClusterLodRenderPass {
    /// Builds the bind-group layout + cut compute pipeline. **Creating the
    /// pipeline validates `cluster_cut.wgsl` on-device** (the GPU driver compiles
    /// it here) — the first on-GPU checkpoint for the per-cluster cut.
    pub async fn new(ctx: &mut RenderPassInitContext<'_>) -> Result<Self> {
        let bind_groups = ClusterCutBindGroups::new(ctx)?;
        let compaction_bind_groups = ClusterCompactionBindGroups::new(ctx)?;
        let pipelines =
            ClusterLodPipelines::new(ctx, &bind_groups, &compaction_bind_groups).await?;
        Ok(Self {
            bind_groups,
            compaction_bind_groups,
            pipelines,
            buffers: None,
            cluster_count: 0,
            render_mesh: None,
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
        indices: &[u32],
    ) -> Result<()> {
        let count = pages.len() as u32;
        let index_count = indices.len() as u32;
        let buffers = match self.buffers.as_mut() {
            Some(b) => {
                b.ensure_capacity(gpu, count, index_count)?;
                b
            }
            None => {
                self.buffers = Some(ClusterLodBuffers::with_capacity(
                    gpu,
                    count.max(1),
                    index_count.max(3),
                )?);
                self.buffers.as_mut().unwrap()
            }
        };
        buffers.write_pages(gpu, pages)?;
        buffers.write_source_indices(gpu, indices)?;
        self.cluster_count = count;
        let buffers = self.buffers.as_ref().unwrap();
        self.bind_groups.recreate(gpu, layouts, buffers)?;
        self.compaction_bind_groups
            .recreate(gpu, layouts, buffers)?;
        Ok(())
    }

    /// Upload the Gap-B residency table (`cluster_id → slot`). Must be called after
    /// [`Self::upload_pages`] (the buffers must exist). No-op if no cluster mesh is
    /// loaded. Only the `cluster_paging` path calls this.
    pub fn upload_resident(&mut self, gpu: &AwsmRendererWebGpu, resident: &[i32]) -> Result<()> {
        if let Some(buffers) = self.buffers.as_mut() {
            buffers.write_resident(gpu, resident)?;
        }
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

    /// Dispatch the compaction: reset the indirect args (index_count→0,
    /// instance_count→1), then pack the selected clusters' index pages into
    /// `compacted_indices` + bump `draw_args.index_count`. Run after [`dispatch`]
    /// (it reads `selected`). After this, `draw_args` drives one
    /// `drawIndexedIndirect(compacted_indices)`.
    pub fn dispatch_compaction(&self, ctx: &RenderContext, first_instance: u32) -> Result<()> {
        let Some(buffers) = self.buffers.as_ref() else {
            return Ok(());
        };
        if self.cluster_count == 0 {
            return Ok(());
        }
        // queue.writeBuffer is ordered before the submitted compute pass.
        buffers.init_draw_args(ctx.gpu, first_instance)?;
        let cp = ctx.command_encoder.begin_compute_pass(Some(
            &ComputePassDescriptor::new(Some("Cluster Compaction")).into(),
        ));
        cp.set_pipeline(ctx.pipelines.compute.get(self.pipelines.compaction)?);
        cp.set_bind_group(0, self.compaction_bind_groups.get_bind_group()?, None)?;
        cp.dispatch_workgroups(
            ClusterLodBuffers::dispatch_groups(self.cluster_count),
            Some(1),
            Some(1),
        );
        cp.end();
        Ok(())
    }
}
