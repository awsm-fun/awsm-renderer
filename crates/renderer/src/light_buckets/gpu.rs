//! GPU storage buffer backing the per-mesh light-indices path.
//!
//! Architecture (post Option F follow-up to Cluster 2.1.c):
//!
//! - **Slice metadata (`offset`, `count`)** lives inside each mesh's
//!   `MaterialMeshMeta` struct at `MATERIAL_MESH_META_LIGHT_SLICE_OFFSET`.
//!   Patched per-frame via `MeshMeta::set_mesh_light_slice` so every
//!   pixel reads it for free as part of the meta load already on its
//!   hot path. Saves one storage-buffer binding.
//! - **`mesh_light_indices[offset..offset+count] -> u32 light_index`**
//!   is the only GPU buffer this module owns. Packed, length == sum of
//!   all slice counts. The struct name reflects that: it owns
//!   *indices*, with the slice metadata living in `MaterialMeshMeta`.
//!
//! Both the slice patches and the indices upload run per-frame, after
//! `LightMeshBuckets::rebuild` and before the material-opaque pass.

use std::sync::LazyLock;

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    renderer::AwsmRendererWebGpu,
};

use crate::lights::Lights;
use crate::meshes::Meshes;

use super::LightMeshBuckets;

static INDICES_BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_storage().with_copy_dst());

/// Owns the `mesh_light_indices` GPU buffer and the per-frame CPU
/// scratch space. Per-mesh slice metadata is patched directly into
/// each mesh's `MaterialMeshMeta` entry instead of living in its own
/// GPU buffer.
pub struct MeshLightIndicesGpu {
    /// `mesh_light_indices` GPU buffer. Packed `u32` light indices.
    /// Read by the lighting shader at `slice.offset .. slice.offset +
    /// slice.count` where the slice fields are loaded from the per-
    /// mesh `MaterialMeshMeta` entry.
    pub indices_buffer: web_sys::GpuBuffer,
    indices_capacity: usize,
    /// Reusable CPU staging buffer.
    indices_scratch: Vec<u8>,
    /// Last-uploaded byte count.
    indices_len: u32,
    /// Number of distinct directional lights uploaded as the global
    /// prefix. The lighting shader applies these to every mesh
    /// unconditionally.
    directional_count: u32,
}

impl MeshLightIndicesGpu {
    /// Creates the indices storage buffer at a small initial capacity.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self, awsm_renderer_core::error::AwsmCoreError> {
        let initial_indices_capacity = 4_usize;
        let indices_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("MeshLightIndices"),
                initial_indices_capacity,
                *INDICES_BUFFER_USAGE,
            )
            .into(),
        )?;
        Ok(Self {
            indices_buffer,
            indices_capacity: initial_indices_capacity,
            indices_scratch: Vec::with_capacity(initial_indices_capacity),
            indices_len: 0,
            directional_count: 0,
        })
    }

    /// Number of valid bytes in the indices buffer this frame.
    pub fn indices_byte_len(&self) -> u32 {
        self.indices_len
    }

    /// Number of directional lights in the global prefix this frame.
    pub fn directional_count(&self) -> u32 {
        self.directional_count
    }

    /// Builds the per-mesh slice + indices for the frame.
    ///
    /// Side effects:
    ///   1. For every mesh in the transpose, calls
    ///      `meshes.meta.set_mesh_light_slice(mesh_key, offset, count)`
    ///      to patch the slice fields inside its `MaterialMeshMeta`
    ///      entry. The dirty-range tracking in `DynamicUniformBuffer`
    ///      coalesces adjacent patches at upload time.
    ///   2. Repacks the indices scratch buffer and uploads it via
    ///      `writeBuffer`. Grows the GPU buffer on capacity miss with
    ///      2x headroom and marks `MeshLightIndicesResize` so the lights
    ///      bind group rebinds the new buffer handle.
    pub fn write_gpu(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        buckets: &LightMeshBuckets,
        lights: &Lights,
        meshes: &mut Meshes,
        bind_groups: &mut crate::bind_groups::BindGroups,
    ) -> Result<(), awsm_renderer_core::error::AwsmCoreError> {
        let per_mesh = buckets.transpose_per_mesh(lights);

        // ── Build indices buffer + patch per-mesh slice fields ────────
        self.indices_scratch.clear();
        // The previous-frame patch needs to be zeroed first for every
        // mesh that's no longer in the bucket — otherwise a mesh that
        // dropped out of every light's range would keep its stale
        // count. Easier than tracking which meshes were patched last
        // frame: walk `per_mesh` directly and patch every mesh in it;
        // the rest stay at whatever they were. For meshes that lose
        // their last light, that means their old `count` stays — a
        // visible bug. Mitigation: zero all slices first.
        //
        // Cheap path: zero the slice fields for every mesh that has a
        // meta entry. The dirty-range mechanism coalesces — runs of
        // zero patches collapse into one write.
        meshes.meta.zero_all_mesh_light_slices();

        for (mesh_key, light_indices) in per_mesh.iter() {
            if light_indices.is_empty() {
                continue;
            }
            let offset_u32 = (self.indices_scratch.len() / 4) as u32;
            let count_u32 = light_indices.len() as u32;
            let landed = meshes
                .meta
                .set_mesh_light_slice(mesh_key, offset_u32, count_u32);
            if !landed {
                // Mesh has no meta slot yet (mid-load). Skip; its
                // light contribution is invisible this frame but
                // becomes visible the frame after the meta lands.
                continue;
            }
            for index in light_indices {
                self.indices_scratch
                    .extend_from_slice(&index.to_le_bytes());
            }
        }

        self.indices_len = self.indices_scratch.len() as u32;
        self.directional_count = buckets.directional_light_indices().len() as u32;

        // ── Grow GPU buffer if needed ────────────────────────────────
        let mut resized = false;
        let mut needed_indices = self.indices_scratch.len().max(4);
        if needed_indices > self.indices_capacity {
            needed_indices = (needed_indices * 2).max(4);
            self.indices_buffer = gpu.create_buffer(
                &BufferDescriptor::new(
                    Some("MeshLightIndices"),
                    needed_indices,
                    *INDICES_BUFFER_USAGE,
                )
                .into(),
            )?;
            self.indices_capacity = needed_indices;
            resized = true;
        }
        if resized {
            bind_groups.mark_create(crate::bind_groups::BindGroupCreate::MeshLightIndicesResize);
        }

        // ── Upload ────────────────────────────────────────────────────
        if !self.indices_scratch.is_empty() {
            gpu.write_buffer(
                &self.indices_buffer,
                None,
                self.indices_scratch.as_slice(),
                None,
                None,
            )?;
        }

        Ok(())
    }
}
