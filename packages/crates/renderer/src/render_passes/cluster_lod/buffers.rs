//! GPU storage buffers backing the cluster-LOD cut pass (Phase B, B.2).
//!
//! Four buffers per cluster mesh:
//! - `pages`: `array<ClusterPage>` (64 B each, [`CLUSTER_PAGE_GPU_STRIDE`]).
//!   Uploaded once at mesh load (the DAG is static).
//! - `selected`: `array<u32>` of length `capacity`. The cut shader writes 1u
//!   (draw this cluster's index page) or 0u. `COPY_SRC` so it can be read back.
//! - `params`: the 96-B `ClusterCutParams` uniform (camera + instance, per frame).
//! - `readback`: `MAP_READ` mirror of `selected`, for verifying the GPU cut
//!   against the CPU [`select_cut_per_cluster`] reference.
//!
//! Inert unless `virtual_geometry` loads a cluster mesh.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

use crate::cluster_lod::{
    write_cluster_cut_params, write_cluster_page_gpu, ClusterPage, CLUSTER_CUT_PARAMS_SIZE,
    CLUSTER_PAGE_GPU_STRIDE,
};
use glam::{Mat4, Vec3};

pub struct ClusterLodBuffers {
    /// `array<ClusterPage>` — the cut shader reads (storage, RO).
    pub pages_buffer: web_sys::GpuBuffer,
    /// `array<u32>` selected flags — cut shader writes (storage, RW + COPY_SRC).
    pub selected_buffer: web_sys::GpuBuffer,
    /// `ClusterCutParams` uniform (camera + instance), rewritten per frame.
    pub params_buffer: web_sys::GpuBuffer,
    /// CPU-mappable mirror of `selected` for readback verification.
    pub readback_buffer: web_sys::GpuBuffer,
    /// Cluster slots both page/selected buffers hold without resizing.
    pub capacity: u32,
    /// Reused page-serialisation scratch (no per-upload allocation).
    staging: Vec<u8>,
}

impl ClusterLodBuffers {
    pub fn with_capacity(
        gpu: &AwsmRendererWebGpu,
        capacity: u32,
    ) -> Result<Self, AwsmCoreError> {
        let capacity = capacity.max(1);
        let pages_bytes = capacity as usize * CLUSTER_PAGE_GPU_STRIDE;
        let selected_bytes = capacity as usize * 4;
        let pages_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("ClusterLodPages"),
                pages_bytes,
                BufferUsage::new().with_storage().with_copy_dst(),
            )
            .into(),
        )?;
        let selected_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("ClusterLodSelected"),
                selected_bytes,
                BufferUsage::new()
                    .with_storage()
                    .with_copy_dst()
                    .with_copy_src(),
            )
            .into(),
        )?;
        let params_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("ClusterLodParams"),
                CLUSTER_CUT_PARAMS_SIZE,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;
        let readback_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("ClusterLodReadback"),
                selected_bytes,
                BufferUsage::new().with_map_read().with_copy_dst(),
            )
            .into(),
        )?;
        Ok(Self {
            pages_buffer,
            selected_buffer,
            params_buffer,
            readback_buffer,
            capacity,
            staging: vec![0u8; pages_bytes],
        })
    }

    /// Grows to hold `needed` clusters (2× headroom). Returns `true` when a
    /// resize happened, so the caller rebuilds the bind group.
    pub fn ensure_capacity(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        needed: u32,
    ) -> Result<bool, AwsmCoreError> {
        if needed <= self.capacity {
            return Ok(false);
        }
        let new_capacity = needed.saturating_mul(2).max(needed);
        *self = Self::with_capacity(gpu, new_capacity)?;
        Ok(true)
    }

    /// Upload the cluster pages (once, at mesh load). Serialises into the reused
    /// scratch via [`write_cluster_page_gpu`], then a single `writeBuffer`.
    pub fn write_pages(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        pages: &[ClusterPage],
    ) -> Result<(), AwsmCoreError> {
        self.staging.clear();
        for p in pages {
            write_cluster_page_gpu(p, &mut self.staging);
        }
        if self.staging.is_empty() {
            return Ok(());
        }
        gpu.write_buffer(&self.pages_buffer, None, self.staging.as_slice(), None, None)
    }

    /// Rewrite the per-frame cut params (camera + this instance's transform).
    #[allow(clippy::too_many_arguments)]
    pub fn write_params(
        &self,
        gpu: &AwsmRendererWebGpu,
        instance_world: &Mat4,
        camera_pos: Vec3,
        tan_half_fov_y: f32,
        viewport_h: f32,
        pixel_budget: f32,
        world_scale: f32,
        cluster_count: u32,
    ) -> Result<(), AwsmCoreError> {
        let mut bytes = Vec::with_capacity(CLUSTER_CUT_PARAMS_SIZE);
        write_cluster_cut_params(
            instance_world,
            camera_pos,
            tan_half_fov_y,
            viewport_h,
            pixel_budget,
            world_scale,
            cluster_count,
            &mut bytes,
        );
        gpu.write_buffer(&self.params_buffer, None, bytes.as_slice(), None, None)
    }

    /// Workgroups to dispatch for `cluster_count` pages at `@workgroup_size(64)`.
    pub fn dispatch_groups(cluster_count: u32) -> u32 {
        cluster_count.div_ceil(64)
    }
}
