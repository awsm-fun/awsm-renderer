//! GPU storage buffers backing the occlusion-cull pass.
//!
//! Two buffers:
//! - `instances`: per-instance occlusion records (world AABB + meta
//!   offset). CPU writes the full active range each frame via
//!   `writeBuffer`. Sized at `INITIAL_CAPACITY` instances and grows
//!   by 2× when the renderables list exceeds capacity.
//! - `visible_this_frame`: `array<u32>` of length `capacity`. The
//!   cull shader writes `1u` or `0u` per instance. The compaction
//!   pass reads this back to gate the geometry-pass survivor split.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// Per-instance struct uploaded to the cull shader. Mirrors
/// `OcclusionInstance` in `cull.wgsl`. Padded to 48 B so the GPU
/// view's stride matches WGSL's natural alignment.
///
/// Layout (offsets in bytes):
/// ```text
///   0..12   world_aabb_min: vec3<f32>
///  12..16   _pad0: u32
///  16..28   world_aabb_max: vec3<f32>
///  28..32   _pad1: u32
///  32..36   mesh_meta_offset: u32      // for cross-ref to MaterialMeshMeta
///  36..40   instance_attr_base: u32    // index into instance attribute buffer
///  40..44   index_count: u32           // static drawIndirect arg, written
///                                      // by compaction shader to the args
///                                      // buffer (was `last_frame_visible`,
///                                      // never read — repurposed so the
///                                      // compaction pass owns the full
///                                      // IndirectDrawArgs layout and the
///                                      // CPU side no longer races against
///                                      // the in-flight geometry pass).
///  44..48   _pad2: u32
/// ```
pub const OCCLUSION_INSTANCE_STRIDE: usize = 48;

/// Starting capacity. Sized for the 1k-mesh tier so the 1× grow path
/// to 2k covers the 1k–4k working set; the 10k-mesh stress scene
/// pays one more 2× grow at first frame.
const INITIAL_CAPACITY: u32 = 1024;

/// GPU buffers for the occlusion cull pass. Pair-managed: the
/// `instances` buffer's size in entries is mirrored by
/// `visible_this_frame`, and `Self::ensure_capacity` grows both
/// together.
pub struct OcclusionBuffers {
    /// Per-instance storage buffer (`array<OcclusionInstance>`).
    /// CPU writes the per-frame active range; cull shader reads.
    pub instances_buffer: web_sys::GpuBuffer,
    /// Per-instance visibility output (`array<u32>`). Cull shader
    /// writes 0/1; the compaction pass reads back.
    pub visible_buffer: web_sys::GpuBuffer,
    /// 16 B uniform carrying `active_count: u32` (+ 12 B pad). The
    /// cull and compaction shaders bound their per-thread loops by
    /// `params.active_count` rather than `arrayLength(&instances)`,
    /// which returns *capacity*. Without this, tail invocations from
    /// the workgroup-rounded dispatch process stale slot data left
    /// over from previous frames and either mark phantom meshes
    /// visible or increment the wrong `IndirectDrawArgs.instance_count`.
    pub params_buffer: web_sys::GpuBuffer,
    /// Number of instance slots both buffers can hold without resizing.
    pub capacity: u32,
    /// Reusable CPU scratch for the per-frame instance staging — sized
    /// to `capacity * stride` and rewritten each frame before upload.
    /// Resized in lockstep with `capacity`.
    pub staging: Vec<u8>,
    /// Mapped-staging-ring uploaders (Phase 2.1). Interior-mutable so
    /// `write_*` keeps its `&self` signature against `render.rs`'s
    /// existing borrow shape.
    pub(crate) instances_uploader:
        std::cell::RefCell<crate::buffer::mapped_uploader::MappedUploader>,
    pub(crate) params_uploader: std::cell::RefCell<crate::buffer::mapped_uploader::MappedUploader>,
}

impl OcclusionBuffers {
    /// Allocates both buffers at `INITIAL_CAPACITY`.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self, AwsmCoreError> {
        Self::with_capacity(gpu, INITIAL_CAPACITY)
    }

    fn with_capacity(gpu: &AwsmRendererWebGpu, capacity: u32) -> Result<Self, AwsmCoreError> {
        let capacity = capacity.max(1);
        let instances_bytes = capacity as usize * OCCLUSION_INSTANCE_STRIDE;
        // 4 bytes per visible_this_frame slot (u32).
        let visible_bytes = capacity as usize * 4;
        let instances_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("OcclusionInstances"),
                instances_bytes,
                BufferUsage::new().with_storage().with_copy_dst(),
            )
            .into(),
        )?;
        let visible_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("OcclusionVisible"),
                visible_bytes,
                BufferUsage::new()
                    .with_storage()
                    .with_copy_dst()
                    .with_copy_src(),
            )
            .into(),
        )?;
        let params_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("OcclusionParams"),
                16,
                BufferUsage::new().with_uniform().with_copy_dst(),
            )
            .into(),
        )?;
        Ok(Self {
            instances_buffer,
            visible_buffer,
            params_buffer,
            capacity,
            staging: vec![0u8; instances_bytes],
            instances_uploader: std::cell::RefCell::new(
                crate::buffer::mapped_uploader::MappedUploader::new("OcclusionInstances"),
            ),
            params_uploader: std::cell::RefCell::new(
                crate::buffer::mapped_uploader::MappedUploader::new("OcclusionParams"),
            ),
        })
    }

    /// Mapped-ring upload telemetry (instances + params aggregated).
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        let mut s = self.instances_uploader.borrow().stats();
        let b = self.params_uploader.borrow().stats();
        s.peak_ring_depth_used = s.peak_ring_depth_used.max(b.peak_ring_depth_used);
        s.fallback_count += b.fallback_count;
        s.map_async_wait_ms += b.map_async_wait_ms;
        s.bytes_uploaded_via_ring += b.bytes_uploaded_via_ring;
        s.bytes_uploaded_via_fallback += b.bytes_uploaded_via_fallback;
        s.bytes_uploaded_via_writebuffer += b.bytes_uploaded_via_writebuffer;
        s.resize_count += b.resize_count;
        s
    }

    /// If `needed > capacity`, reallocates both buffers (and resets
    /// the scratch) to `needed * 2`. Returns `true` when a resize
    /// happened so the caller can rebuild dependent bind groups.
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

    /// Writes this frame's active instance count into the params
    /// uniform. The cull + compaction shaders bound their per-thread
    /// `if (i >= count)` against this rather than `arrayLength`, so
    /// tail invocations don't read or mark stale slot data.
    pub fn write_params(
        &self,
        gpu: &AwsmRendererWebGpu,
        active_count: u32,
    ) -> Result<(), AwsmCoreError> {
        // Stack-allocated — this is a per-frame hot path, so the heap
        // `vec![0u8; 16]` it used to do was pure churn for a
        // fixed-size 16-byte uniform (4-byte count + 12-byte pad).
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&active_count.to_le_bytes());
        self.params_uploader.borrow_mut().write_dirty_ranges(
            gpu,
            &self.params_buffer,
            16,
            &bytes,
            &[(0, 16)],
        )
    }

    /// Upload the per-frame instances payload via the mapped ring.
    /// `bytes` is the active prefix (`count * stride`); the dest
    /// buffer is sized to the full capacity.
    pub fn write_instances(
        &self,
        gpu: &AwsmRendererWebGpu,
        bytes: &[u8],
    ) -> Result<(), AwsmCoreError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let dest_size = self.capacity as usize * OCCLUSION_INSTANCE_STRIDE;
        let n = bytes.len();
        self.instances_uploader.borrow_mut().write_dirty_ranges(
            gpu,
            &self.instances_buffer,
            dest_size,
            bytes,
            &[(0, n)],
        )
    }
}
