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

/// `drawIndexedIndirect` args: 5 × u32 (index_count, instance_count, first_index,
/// base_vertex, first_instance).
pub const CLUSTER_DRAW_ARGS_SIZE: usize = 20;

pub struct ClusterLodBuffers {
    /// `array<ClusterPage>` — the cut shader reads (storage, RO).
    pub pages_buffer: web_sys::GpuBuffer,
    /// `array<u32>` selected flags — cut shader writes (storage, RW + COPY_SRC).
    pub selected_buffer: web_sys::GpuBuffer,
    /// `ClusterCutParams` uniform (camera + instance), rewritten per frame.
    pub params_buffer: web_sys::GpuBuffer,
    /// CPU-mappable mirror of `selected` for readback verification.
    pub readback_buffer: web_sys::GpuBuffer,
    /// The mesh's full concatenated cluster index pages (`array<u32>`); the
    /// compaction reads selected clusters' slices out of this (storage, RO).
    pub source_indices_buffer: web_sys::GpuBuffer,
    /// Compaction output: the selected clusters' indices packed contiguously
    /// (storage RW for the compaction + INDEX for the draw).
    pub compacted_indices_buffer: web_sys::GpuBuffer,
    /// `drawIndexedIndirect` args the compaction fills (storage RW for the
    /// atomic index_count + INDIRECT for the draw + COPY_DST/SRC for clear/read).
    pub draw_args_buffer: web_sys::GpuBuffer,
    /// Cluster slots both page/selected buffers hold without resizing.
    pub capacity: u32,
    /// Index slots the source/compacted index buffers hold without resizing.
    pub index_capacity: u32,
    /// Reused page-serialisation scratch (no per-upload allocation).
    staging: Vec<u8>,
}

impl ClusterLodBuffers {
    pub fn with_capacity(
        gpu: &AwsmRendererWebGpu,
        capacity: u32,
        index_capacity: u32,
    ) -> Result<Self, AwsmCoreError> {
        let capacity = capacity.max(1);
        let index_capacity = index_capacity.max(3);
        let pages_bytes = capacity as usize * CLUSTER_PAGE_GPU_STRIDE;
        let selected_bytes = capacity as usize * 4;
        let index_bytes = index_capacity as usize * 4;
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
        let source_indices_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("ClusterLodSourceIndices"),
                index_bytes,
                BufferUsage::new().with_storage().with_copy_dst(),
            )
            .into(),
        )?;
        let compacted_indices_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("ClusterLodCompactedIndices"),
                index_bytes,
                BufferUsage::new()
                    .with_storage()
                    .with_index()
                    .with_copy_src(),
            )
            .into(),
        )?;
        let draw_args_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("ClusterLodDrawArgs"),
                CLUSTER_DRAW_ARGS_SIZE,
                BufferUsage::new()
                    .with_storage()
                    .with_indirect()
                    .with_copy_dst()
                    .with_copy_src(),
            )
            .into(),
        )?;
        Ok(Self {
            pages_buffer,
            selected_buffer,
            params_buffer,
            readback_buffer,
            source_indices_buffer,
            compacted_indices_buffer,
            draw_args_buffer,
            capacity,
            index_capacity,
            staging: vec![0u8; pages_bytes],
        })
    }

    /// Grows to hold `needed` clusters / `needed_indices` indices (2× headroom).
    /// Returns `true` when a resize happened, so the caller rebuilds the bind
    /// group.
    pub fn ensure_capacity(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        needed: u32,
        needed_indices: u32,
    ) -> Result<bool, AwsmCoreError> {
        if needed <= self.capacity && needed_indices <= self.index_capacity {
            return Ok(false);
        }
        let new_capacity = needed.saturating_mul(2).max(needed).max(self.capacity);
        let new_index_capacity = needed_indices
            .saturating_mul(2)
            .max(needed_indices)
            .max(self.index_capacity);
        *self = Self::with_capacity(gpu, new_capacity, new_index_capacity)?;
        Ok(true)
    }

    /// Upload the mesh's full concatenated cluster index pages (once, at load).
    pub fn write_source_indices(
        &self,
        gpu: &AwsmRendererWebGpu,
        indices: &[u32],
    ) -> Result<(), AwsmCoreError> {
        if indices.is_empty() {
            return Ok(());
        }
        // One-shot at load (not per-frame); a single staging Vec is fine.
        let mut bytes = Vec::with_capacity(indices.len() * 4);
        for &i in indices {
            bytes.extend_from_slice(&i.to_le_bytes());
        }
        gpu.write_buffer(
            &self.source_indices_buffer,
            None,
            bytes.as_slice(),
            None,
            None,
        )
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
        gpu.write_buffer(
            &self.pages_buffer,
            None,
            self.staging.as_slice(),
            None,
            None,
        )
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

    /// Reset the indirect draw args to `{index_count:0, instance_count:1,
    /// first_index:0, base_vertex:0, first_instance}`. The compaction atomic-bumps
    /// `index_count`; `first_instance` carries the cluster render mesh's
    /// `mesh_meta_idx` so the geometry vertex shader's
    /// `geometry_mesh_metas[instance_index]` resolves to it (material routing).
    pub fn init_draw_args(
        &self,
        gpu: &AwsmRendererWebGpu,
        first_instance: u32,
    ) -> Result<(), AwsmCoreError> {
        let mut bytes = [0u8; CLUSTER_DRAW_ARGS_SIZE];
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes()); // instance_count = 1
        bytes[16..20].copy_from_slice(&first_instance.to_le_bytes());
        gpu.write_buffer(&self.draw_args_buffer, None, bytes.as_slice(), None, None)
    }

    /// Workgroups to dispatch for `cluster_count` pages at `@workgroup_size(64)`.
    pub fn dispatch_groups(cluster_count: u32) -> u32 {
        cluster_count.div_ceil(64)
    }
}
