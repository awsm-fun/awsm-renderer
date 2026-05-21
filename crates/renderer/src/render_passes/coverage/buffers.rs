//! Storage + readback buffers for the GPU mesh-coverage producer.
//!
//! Layout:
//! - `counts_buffer`: `STORAGE | COPY_SRC | COPY_DST`, one `u32`
//!   per mesh slot. The compute shader atomic-adds into this each
//!   frame; the renderer zeros it before the dispatch.
//! - `readback_buffer`: `MAP_READ | COPY_DST`. Each frame we
//!   `copyBufferToBuffer(counts → readback)` and kick off a
//!   `mapAsync` that resolves on a future frame. The mapped bytes
//!   feed [`MeshCoverage::ingest`].
//!
//! Single-buffer (not ringed) readback path: the renderer drops the
//! frame's readback if a prior frame's `mapAsync` is still in
//! flight. One-frame latency is the plan's contract; dropping
//! occasional frames under high mapping latency keeps the path
//! deterministic without a buffer-ring complication.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// 4 bytes per mesh slot (one `u32` count).
pub const COUNTS_STRIDE_BYTES: usize = 4;

/// Starting slot capacity. Grows 2× via `ensure_capacity` when
/// `meshes.len()` exceeds it.
const INITIAL_CAPACITY: u32 = 1024;

pub struct CoverageBuffers {
    /// Storage buffer the compute pass atomic-adds into. One u32
    /// per mesh slot; matches the indexing of the §16.7/§16.8 args
    /// buffer (`mesh_meta_offset / 256`).
    pub counts_buffer: web_sys::GpuBuffer,
    /// CPU-mappable readback. The renderer's
    /// `copyBufferToBuffer(counts → readback)` runs each frame; a
    /// `mapAsync` then resolves with last-frame's counts.
    pub readback_buffer: web_sys::GpuBuffer,
    /// CPU-side zero buffer used to clear `counts_buffer` each
    /// frame (writeBuffer of zeros, sized to `capacity * 4` bytes).
    /// Reallocated on capacity changes.
    pub zero_scratch: Vec<u8>,
    pub capacity: u32,
}

impl CoverageBuffers {
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self, AwsmCoreError> {
        Self::with_capacity(gpu, INITIAL_CAPACITY)
    }

    fn with_capacity(gpu: &AwsmRendererWebGpu, capacity: u32) -> Result<Self, AwsmCoreError> {
        let capacity = capacity.max(1);
        let size_bytes = capacity as usize * COUNTS_STRIDE_BYTES;
        let counts_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("CoverageCounts"),
                size_bytes,
                BufferUsage::new()
                    .with_storage()
                    .with_copy_src()
                    .with_copy_dst(),
            )
            .into(),
        )?;
        let readback_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("CoverageReadback"),
                size_bytes,
                BufferUsage::new().with_map_read().with_copy_dst(),
            )
            .into(),
        )?;
        Ok(Self {
            counts_buffer,
            readback_buffer,
            zero_scratch: vec![0u8; size_bytes],
            capacity,
        })
    }

    /// Grows both buffers when the mesh slot count exceeds capacity.
    /// Returns `true` when reallocated (caller marks the matching
    /// bind groups dirty).
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

    /// Zero the counts buffer for this frame. The compute pass
    /// atomic-adds on top; without the reset the counts would
    /// accumulate across frames.
    pub fn reset_counts(&self, gpu: &AwsmRendererWebGpu) -> Result<(), AwsmCoreError> {
        gpu.write_buffer(
            &self.counts_buffer,
            None,
            self.zero_scratch.as_slice(),
            None,
            None,
        )
    }
}
