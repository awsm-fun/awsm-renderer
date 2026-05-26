//! GPU storage buffer backing the classify pass's per-`shader_id`
//! tile buckets and indirect-dispatch args.
//!
//! Single buffer holds:
//! - One [`DispatchIndirectArgs`]-shaped slot per registered bucket
//!   (first-party + dynamic) at the start — written atomically by
//!   classify, read by the driver via `dispatchWorkgroupsIndirect`.
//! - Per-bucket starting offsets and the shared per-bucket capacity.
//! - A packed `array<vec2<u32>>` of tile coordinates, partitioned by
//!   bucket. Each tile is `(workgroup_id_x, workgroup_id_y)`; the
//!   material pass reads it back as `tile_xy * 8u + local_id.xy →
//!   pixel coords`.
//!
//! The buffer is re-created when the viewport size changes (the
//! capacity depends on the tile count) OR when the bucket count
//! changes (a dynamic-material registration grew the total). The
//! header is rewritten each frame to reset the atomic counters; the
//! tile array is overwritten by classify in-place.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// Bytes per indirect-args entry: `(x: u32, y: u32, z: u32, _pad: u32)`.
/// `bucket_count` of these laid out back-to-back form the buffer
/// prefix that `dispatchWorkgroupsIndirect` reads from at the offset
/// returned by [`indirect_args_offset`].
pub const INDIRECT_ARGS_STRIDE: u32 = 16;

/// Header byte count given a bucket count. Layout:
///   - `bucket_count` × `INDIRECT_ARGS_STRIDE` bytes of indirect args
///   - `bucket_count` × `u32` bytes of per-bucket tile offsets
///   - 1 × `u32` for the shared bucket_capacity
///   - 12 bytes of alignment padding so the trailing `tiles` array
///     (`vec2<u32>`, 8-byte stride) starts 16-byte aligned.
///
/// The header is laid out by [`write_header`] in the same order the
/// templated WGSL `ClassifyOutput` struct declares its fields.
pub fn header_bytes(bucket_count: u32) -> u32 {
    let args_bytes = bucket_count * INDIRECT_ARGS_STRIDE;
    let offsets_bytes = bucket_count * 4;
    let capacity_bytes = 4;
    let unpadded = args_bytes + offsets_bytes + capacity_bytes;
    // Round up to 16-byte alignment so `array<vec2<u32>>` starts cleanly.
    (unpadded + 15) & !15
}

/// Single storage buffer holding indirect args + tile buckets for the
/// opaque classify pass. Sized to the current viewport's tile count
/// AND the current bucket count; recreated by
/// [`ClassifyBuffers::ensure_capacity`] on resize or by
/// [`ClassifyBuffers::ensure_bucket_count`] when a dynamic material
/// registration grows the registry.
pub struct ClassifyBuffers {
    /// Storage + indirect + copy-dst GPU buffer. Bound read-write to
    /// classify; bound read-only to material-opaque (different declared
    /// struct types avoid WGSL's atomic-in-read-only restriction).
    /// Also passed to `dispatchWorkgroupsIndirect` at offsets
    /// computed by [`indirect_args_offset`].
    pub buffer: web_sys::GpuBuffer,
    /// Per-bucket capacity in tile entries.
    pub bucket_capacity: u32,
    /// Number of opaque-classify buckets (first-party + currently-
    /// registered dynamic materials).
    pub bucket_count: u32,
    /// Total buffer size in bytes, including header + bucket_count ×
    /// bucket_capacity worth of tile entries.
    pub size_bytes: u32,
    /// CPU staging for the per-frame header reset. Re-uploaded to the
    /// buffer's first `header_bytes(bucket_count)` bytes at the top of
    /// every frame — zeros the atomic counters and re-asserts the
    /// per-bucket offsets / capacity so the classify shader runs
    /// against a clean header.
    header_scratch: Vec<u8>,
}

impl ClassifyBuffers {
    /// Creates the classify buffer sized to `(bucket_capacity, bucket_count)`.
    /// Cheap on small scenes — the upfront allocation amortizes vs
    /// growing on first frame.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        bucket_capacity: u32,
        bucket_count: u32,
    ) -> Result<Self, AwsmCoreError> {
        let bucket_capacity = bucket_capacity.max(1);
        let bucket_count = bucket_count.max(1);
        let header = header_bytes(bucket_count);
        let tiles_bytes = bucket_capacity
            .saturating_mul(bucket_count)
            .saturating_mul(8);
        let size_bytes = header + tiles_bytes;

        let buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("MaterialClassifyBuckets"),
                size_bytes as usize,
                BufferUsage::new()
                    .with_storage()
                    .with_indirect()
                    .with_copy_dst(),
            )
            .into(),
        )?;

        let mut header_scratch = vec![0u8; header as usize];
        write_header(&mut header_scratch, bucket_capacity, bucket_count);

        Ok(Self {
            buffer,
            bucket_capacity,
            bucket_count,
            size_bytes,
            header_scratch,
        })
    }

    /// Recreates the buffer if the viewport tile count exceeds the
    /// current capacity. Called from the render path before each
    /// classify dispatch. Returns `true` if the buffer was recreated
    /// (caller rebuilds dependent bind groups).
    pub fn ensure_capacity(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        needed_capacity: u32,
    ) -> Result<bool, AwsmCoreError> {
        if needed_capacity <= self.bucket_capacity {
            return Ok(false);
        }
        // Grow with 2× headroom so back-to-back resizes don't thrash.
        let new_capacity = (needed_capacity * 2).max(needed_capacity);
        *self = Self::new(gpu, new_capacity, self.bucket_count)?;
        Ok(true)
    }

    /// Recreates the buffer if a dynamic-material registration grew
    /// the registry past the current `bucket_count`. Returns `true` if
    /// the buffer was recreated (caller rebuilds dependent bind groups
    /// AND the templated classify pipeline since the shader source
    /// changes when bucket_count does).
    pub fn ensure_bucket_count(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        needed_bucket_count: u32,
    ) -> Result<bool, AwsmCoreError> {
        if needed_bucket_count <= self.bucket_count {
            return Ok(false);
        }
        *self = Self::new(gpu, self.bucket_capacity, needed_bucket_count)?;
        Ok(true)
    }

    /// Writes the per-frame header reset into the buffer: zeroes the
    /// per-bucket `workgroup_count_x` atomics, re-asserts `(y=1, z=1)`,
    /// and re-emits the bucket offsets + capacity. The tile array tail
    /// is left alone — classify overwrites each entry it appends.
    pub fn reset_header(&self, gpu: &AwsmRendererWebGpu) -> Result<(), AwsmCoreError> {
        gpu.write_buffer(
            &self.buffer,
            None,
            self.header_scratch.as_slice(),
            None,
            None,
        )
    }
}

/// Layouts the header for `bucket_count` buckets.
///
/// Layout order (matches the templated WGSL `ClassifyOutput` struct):
///   1. `bucket_count` × `ClassifyIndirectArgs { x:0, y:1, z:1, _pad:0 }`
///   2. `bucket_count` × per-bucket starting offset into the
///      `tiles` array (in entry-count units, each entry 8 B).
///      Bucket `i` starts at `i * bucket_capacity`.
///   3. 1 × `bucket_capacity` u32, shared across all buckets.
///   4. Alignment padding so the trailing `tiles` array starts
///      16-byte aligned.
pub fn write_header(dst: &mut [u8], bucket_capacity: u32, bucket_count: u32) {
    let one = 1u32.to_ne_bytes();
    // 1. Indirect args.
    for bucket in 0..bucket_count as usize {
        let base = bucket * INDIRECT_ARGS_STRIDE as usize;
        dst[base..base + 4].copy_from_slice(&[0; 4]); // x
        dst[base + 4..base + 8].copy_from_slice(&one); // y
        dst[base + 8..base + 12].copy_from_slice(&one); // z
        dst[base + 12..base + 16].copy_from_slice(&[0; 4]); // _pad
    }
    // 2. Per-bucket offsets.
    let offsets_base = (bucket_count * INDIRECT_ARGS_STRIDE) as usize;
    for bucket in 0..bucket_count as usize {
        let off = (bucket as u32).saturating_mul(bucket_capacity);
        let dst_base = offsets_base + bucket * 4;
        dst[dst_base..dst_base + 4].copy_from_slice(&off.to_ne_bytes());
    }
    // 3. bucket_capacity.
    let cap_base = offsets_base + (bucket_count * 4) as usize;
    dst[cap_base..cap_base + 4].copy_from_slice(&bucket_capacity.to_ne_bytes());
    // 4. Alignment padding — left at zero from the initial vec![0u8; …].
}

/// Indirect-args byte offset for a given bucket index. Passed as the
/// second arg to `dispatch_workgroups_indirect` on the material-opaque
/// pipeline matching that bucket's shader_id.
pub fn indirect_args_offset(bucket_index: u32) -> u32 {
    bucket_index * INDIRECT_ARGS_STRIDE
}
