//! GPU storage buffer backing the classify pass's per-`shader_id`
//! tile buckets and indirect-dispatch args.
//!
//! Single buffer holds:
//! - One [`DispatchIndirectArgs`]-shaped slot per shader_id (PBR /
//!   Unlit / Toon) at the start — written atomically by classify,
//!   read by the driver via `dispatchWorkgroupsIndirect`.
//! - Per-bucket starting offsets and the per-bucket capacity.
//! - A packed `array<vec2<u32>>` of tile coordinates, partitioned by
//!   bucket. Each tile is `(workgroup_id_x, workgroup_id_y)`; the
//!   material pass reads it back as `tile_xy * 8u + local_id.xy →
//!   pixel coords`.
//!
//! The buffer is re-created when the viewport size changes (the
//! capacity depends on the tile count). The header is rewritten each
//! frame to reset the atomic counters; the tile array is overwritten
//! by classify in-place.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// Bytes per indirect-args entry: `(x: u32, y: u32, z: u32, _pad: u32)`.
/// [`BUCKET_COUNT`] of these laid out back-to-back form the buffer
/// prefix that `dispatchWorkgroupsIndirect` reads from at offsets
/// 0, 16, 32, 48 (one per opaque shader_id).
pub const INDIRECT_ARGS_STRIDE: u32 = 16;

/// Header byte count: 4 × indirect args (64 B) + 4 bucket offsets +
/// capacity = 84 B, rounded up to 96 B for vec2<u32> alignment on the
/// trailing tile array. The tile array starts at this offset.
pub const HEADER_BYTES: u32 = 96;

/// Number of opaque-classify buckets — one per opaque
/// [`MaterialShaderId`] variant: PBR (0), Unlit (1), Toon (2),
/// FlipBook (3). Must stay in lockstep with the askama template's
/// `shader_id_bucket` emit in `material_classify_wgsl/compute.wgsl`.
pub const BUCKET_COUNT: u32 = 4;

/// Single storage buffer holding indirect args + tile buckets for the
/// opaque classify pass. Sized to the current viewport's tile count;
/// recreated by [`ClassifyBuffers::ensure_capacity`] on resize.
pub struct ClassifyBuffers {
    /// Storage + indirect + copy-dst GPU buffer. Bound read-write to
    /// classify; bound read-only to material-opaque (different declared
    /// struct types avoid WGSL's atomic-in-read-only restriction).
    /// Also passed to `dispatchWorkgroupsIndirect` at offsets 0/16/32.
    pub buffer: web_sys::GpuBuffer,
    /// Per-bucket capacity in tile entries.
    pub bucket_capacity: u32,
    /// Total buffer size in bytes, including header + 3 × bucket
    /// capacity worth of tile entries.
    pub size_bytes: u32,
    /// CPU staging for the per-frame header reset. Re-uploaded to the
    /// buffer's first [`HEADER_BYTES`] bytes at the top of every
    /// frame — zeros the atomic counters and re-asserts the
    /// per-bucket offsets / capacity so the classify shader runs
    /// against a clean header.
    header_scratch: [u8; HEADER_BYTES as usize],
}

impl ClassifyBuffers {
    /// Creates the classify buffer sized to a tile-count-per-bucket of
    /// `capacity`. Cheap on a small viewport (~megabytes) — the
    /// upfront allocation amortizes vs growing on first frame.
    pub fn new(gpu: &AwsmRendererWebGpu, bucket_capacity: u32) -> Result<Self, AwsmCoreError> {
        let bucket_capacity = bucket_capacity.max(1);
        let tiles_bytes = bucket_capacity
            .saturating_mul(BUCKET_COUNT)
            .saturating_mul(8);
        let size_bytes = HEADER_BYTES + tiles_bytes;

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

        let mut header_scratch = [0u8; HEADER_BYTES as usize];
        write_header(&mut header_scratch, bucket_capacity);

        Ok(Self {
            buffer,
            bucket_capacity,
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
        *self = Self::new(gpu, new_capacity)?;
        Ok(true)
    }

    /// Writes the per-frame header reset into the buffer: zeroes the
    /// three `workgroup_count_x` atomics, re-asserts `(y=1, z=1)`, and
    /// re-emits the bucket offsets + capacity. The tile array tail is
    /// left alone — classify overwrites each entry it appends.
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

fn write_header(dst: &mut [u8; HEADER_BYTES as usize], bucket_capacity: u32) {
    // Indirect-args entries — `(x=0, y=1, z=1, _pad=0)`. The atomic
    // `x` counter increments to the workgroup count as classify
    // discovers tiles for each bucket.
    let one = 1u32.to_ne_bytes();
    for bucket in 0..BUCKET_COUNT as usize {
        let base = bucket * INDIRECT_ARGS_STRIDE as usize;
        dst[base..base + 4].copy_from_slice(&[0; 4]); // x
        dst[base + 4..base + 8].copy_from_slice(&one); // y
        dst[base + 8..base + 12].copy_from_slice(&one); // z
        dst[base + 12..base + 16].copy_from_slice(&[0; 4]); // _pad
    }
    // Per-bucket starting offset into the `tiles` array, in
    // entry-count units (each entry is `vec2<u32>` = 8 bytes). PBR=0,
    // Unlit=cap, Toon=2*cap, FlipBook=3*cap.
    let base = (BUCKET_COUNT * INDIRECT_ARGS_STRIDE) as usize;
    dst[base..base + 4].copy_from_slice(&0u32.to_ne_bytes());
    dst[base + 4..base + 8].copy_from_slice(&bucket_capacity.to_ne_bytes());
    dst[base + 8..base + 12].copy_from_slice(&bucket_capacity.saturating_mul(2).to_ne_bytes());
    dst[base + 12..base + 16].copy_from_slice(&bucket_capacity.saturating_mul(3).to_ne_bytes());
    // bucket_capacity (shared across all buckets) follows the four offsets.
    dst[base + 16..base + 20].copy_from_slice(&bucket_capacity.to_ne_bytes());
    // The remaining bytes up to HEADER_BYTES are alignment padding —
    // unused by the shader, left at zero.
}

/// Indirect-args byte offset for a given bucket index (0=PBR,
/// 1=Unlit, 2=Toon, 3=FlipBook). Passed as the second arg to
/// `dispatch_workgroups_indirect` on the material-opaque pipeline
/// matching that shader_id.
pub fn indirect_args_offset(bucket_index: u32) -> u32 {
    bucket_index * INDIRECT_ARGS_STRIDE
}
