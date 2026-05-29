//! Per-mesh light-indices CPU build path.
//!
//! Architecture:
//!
//! - **Slice metadata (`offset`, `count`)** lives inside each mesh's
//!   `MaterialMeshMeta` struct at `MATERIAL_MESH_META_LIGHT_SLICE_OFFSET`.
//!   Patched per-frame via `MeshMeta::set_mesh_light_slice` so every
//!   pixel reads it for free as part of the meta load already on its
//!   hot path. Saves one storage-buffer binding.
//! - **Per-mesh light indices** are written into the **head region** of
//!   the shared `LightCullingBuffers::storage_buffer` (offset 0,
//!   length `mesh_indices_capacity_u32`). The buffer's tail region
//!   carries the GPU cull pass's froxel data — merging the two regions
//!   into a single storage binding keeps the opaque pass under WebGPU's
//!   `maxStorageBuffersPerShaderStage` ceiling.
//! - **Oversized meshes** (AABB diagonal >50 m AND bucket size >16
//!   lights) get a sentinel `light_slice_count = 0xFFFFFFFF` so the
//!   opaque shader takes the per-pixel froxel walk instead of looping
//!   through a coarse per-mesh slice.
//!
//! Both the slice patches and the indices upload run per-frame, after
//! `LightMeshBuckets::rebuild` and before the material-opaque pass.

use awsm_renderer_core::renderer::AwsmRendererWebGpu;

use crate::lights::Lights;
use crate::meshes::Meshes;
use crate::render_passes::light_culling::LightCullingBuffers;

use super::LightMeshBuckets;

/// `light_slice_count` sentinel: signals to the opaque shader that this
/// mesh routes through the per-pixel froxel walk instead of consuming
/// per-mesh slice indices.
pub const OVERSIZED_SLICE_SENTINEL: u32 = 0xFFFFFFFF;

/// Per-frame CPU build state for the per-mesh light-indices region of
/// the shared `LightCullingBuffers::storage_buffer`.
pub struct MeshLightIndicesGpu {
    /// Reusable CPU staging buffer for the per-frame indices payload.
    indices_scratch: Vec<u8>,
    /// Last-uploaded byte count.
    indices_len: u32,
    /// Number of distinct directional lights uploaded as the global
    /// prefix. The lighting shader applies these to every mesh
    /// unconditionally.
    directional_count: u32,
    /// `true` if the prior frame uploaded any per-mesh light slices.
    /// Gates the per-frame `zero_all_mesh_light_slices` walk: when
    /// both the prior frame *and* this frame have no per-mesh entries,
    /// every slice in `MaterialMeshMeta` is already zero and the walk
    /// is a no-op write fest over every registered mesh.
    prior_frame_had_per_mesh: bool,
    uploader: crate::buffer::mapped_uploader::MappedUploader,
}

impl MeshLightIndicesGpu {
    /// Creates an empty mesh light indices builder. The actual storage
    /// buffer lives on `LightCullingBuffers` — this struct just owns
    /// scratch + uploader state.
    pub fn new(
        _gpu: &AwsmRendererWebGpu,
    ) -> Result<Self, awsm_renderer_core::error::AwsmCoreError> {
        Ok(Self {
            indices_scratch: Vec::with_capacity(64),
            indices_len: 0,
            directional_count: 0,
            prior_frame_had_per_mesh: false,
            uploader: crate::buffer::mapped_uploader::MappedUploader::new("MeshLightIndices"),
        })
    }

    /// Mapped-ring upload telemetry for the indices uploader.
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.uploader.stats()
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
    ///      entry. Oversized meshes get
    ///      `(0, OVERSIZED_SLICE_SENTINEL)` so the opaque shader
    ///      routes them through the per-pixel froxel walk.
    ///   2. Repacks the indices scratch buffer and uploads it via
    ///      `writeBuffer` into the head region of
    ///      `light_culling_buffers.storage_buffer`. Grows the buffer
    ///      via `LightCullingBuffers::ensure_mesh_indices_capacity`
    ///      on capacity miss and marks `LightCullingFroxelsResize` so
    ///      the lights bind groups rebind the new buffer handle.
    pub fn write_gpu(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        buckets: &LightMeshBuckets,
        lights: &Lights,
        meshes: &mut Meshes,
        light_culling_buffers: &mut LightCullingBuffers,
        bind_groups: &mut crate::bind_groups::BindGroups,
    ) -> Result<(), awsm_renderer_core::error::AwsmCoreError> {
        let per_mesh = buckets.transpose_per_mesh(lights);
        let has_per_mesh = !per_mesh.is_empty();
        let has_oversized = buckets.has_oversized();

        // Fast path for scenes with no overlapping point / spot lights
        // and no oversized meshes. The slice fields in every mesh's
        // `MaterialMeshMeta` are already zero, so there's nothing to
        // patch this frame. Bumping the `directional_count` is still
        // required — that prefix size can change frame-to-frame.
        if !self.prior_frame_had_per_mesh && !has_per_mesh && !has_oversized {
            self.indices_len = 0;
            self.directional_count = buckets.directional_light_indices().len() as u32;
            return Ok(());
        }

        // ── Build indices buffer + patch per-mesh slice fields ────────
        self.indices_scratch.clear();
        // The previous-frame patch needs to be zeroed first for every
        // mesh that's no longer in the bucket — otherwise a mesh that
        // dropped out of every light's range would keep its stale
        // count. Walk every mesh that has a meta entry and write zero;
        // the dirty-range mechanism coalesces runs of adjacent zero
        // patches into one buffer write.
        meshes.meta.zero_all_mesh_light_slices();

        // Oversized meshes route to the per-pixel froxel walk. Write
        // the sentinel into their `MaterialMeshMeta.light_slice_count`
        // before walking the per-mesh transpose so the per-mesh entry
        // doesn't overwrite the sentinel.
        let oversized: std::collections::HashSet<crate::meshes::MeshKey> =
            buckets.oversized_meshes().iter().copied().collect();
        for &mesh_key in buckets.oversized_meshes() {
            meshes
                .meta
                .set_mesh_light_slice(mesh_key, 0, OVERSIZED_SLICE_SENTINEL);
        }

        for (mesh_key, light_indices) in per_mesh.iter() {
            if oversized.contains(&mesh_key) {
                // Already wrote the sentinel above. Don't burn indices
                // on a per-mesh slice the shader won't read.
                continue;
            }
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
                self.indices_scratch.extend_from_slice(&index.to_le_bytes());
            }
        }

        self.indices_len = self.indices_scratch.len() as u32;
        self.directional_count = buckets.directional_light_indices().len() as u32;

        // ── Grow merged storage if needed ────────────────────────────
        let needed_u32 = (self.indices_scratch.len() / 4) as u32;
        let resized = light_culling_buffers.ensure_mesh_indices_capacity(gpu, needed_u32)?;
        if resized {
            bind_groups.mark_create(crate::bind_groups::BindGroupCreate::LightCullingFroxelsResize);
        }

        // ── Upload into the head region of the shared buffer ─────────
        //
        // Crucial: `MappedStagingRing` allocates staging slots sized to
        // `dest_size × ring_depth` with `mappedAtCreation: true`, and
        // Chrome enforces a device-wide pool limit on those. The merged
        // `storage_buffer` is tens of MB, but the CPU only ever writes
        // the head region (per-mesh light indices) — the cull pass
        // writes the froxel tail through shader-side atomics, no host
        // upload. If we passed the full buffer size here, the ring
        // would allocate 3 × tens of MB of mapped staging and exhaust
        // the pool, breaking unrelated mapped uploads (shadow
        // descriptors, etc.) device-wide. Cap `dest_size` to the head
        // region.
        if !self.indices_scratch.is_empty() {
            let n = self.indices_scratch.len();
            let head_region_bytes = light_culling_buffers.mesh_indices_capacity_u32 as usize * 4;
            self.uploader.write_dirty_ranges(
                gpu,
                &light_culling_buffers.storage_buffer,
                head_region_bytes.max(n),
                self.indices_scratch.as_slice(),
                &[(0, n)],
            )?;
        }

        self.prior_frame_had_per_mesh = has_per_mesh || has_oversized;

        Ok(())
    }
}
