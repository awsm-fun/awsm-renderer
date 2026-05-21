//! GPU storage buffer for the decal classify pass's per-tile buckets.
//!
//! Layout (header in u32 units, then a flat per-tile region):
//! ```text
//!   0..4    tile_count_x: u32        // ceil(viewport_w / 8)
//!   4..8    tile_count_y: u32        // ceil(viewport_h / 8)
//!   8..12   bucket_capacity: u32     // entries-per-tile cap
//!  12..16   _pad: u32
//!  per-tile (stride = (1 + bucket_capacity) × 4 B):
//!    +0     atomic<u32> count        // atomic-append cursor
//!    +4..   array<u32, bucket_capacity> entries  // decal indices
//! ```
//!
//! Re-allocated on viewport resize via `ensure_capacity`. Per-frame,
//! `reset_header` zeros the atomic counts so a fresh classify starts
//! against an empty bucket set.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// Header byte count (4 × u32).
pub const HEADER_BYTES: u32 = 16;

/// Per-tile entry cap. At 4K (480×270 tiles = 130K), a 32-cap bucket
/// costs `130K × 33 × 4 B = ~17 MB`. Bump if scenes routinely overflow.
pub const BUCKET_CAPACITY: u32 = 32;

/// Per-tile stride in bytes (`atomic<u32>` count + `BUCKET_CAPACITY × u32`).
pub const PER_TILE_BYTES: u32 = 4 + BUCKET_CAPACITY * 4;

pub struct DecalClassifyBuffers {
    /// Storage + copy_dst buffer holding the per-tile bucket region.
    pub buffer: web_sys::GpuBuffer,
    pub tile_count_x: u32,
    pub tile_count_y: u32,
    pub size_bytes: u32,
    /// CPU staging for the per-frame header reset — re-uploads the
    /// header constants (tile_count_x, tile_count_y, bucket_capacity)
    /// + zeros the atomic counts in the tile region tail. The
    /// allocation is dominated by the zero tail.
    header_scratch: Vec<u8>,
}

impl DecalClassifyBuffers {
    /// Creates a buffer sized for a 1×1 tile grid; `ensure_capacity`
    /// resizes on the first frame against the real viewport.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self, AwsmCoreError> {
        Self::with_tile_count(gpu, 1, 1)
    }

    fn with_tile_count(
        gpu: &AwsmRendererWebGpu,
        tile_count_x: u32,
        tile_count_y: u32,
    ) -> Result<Self, AwsmCoreError> {
        let tile_count_x = tile_count_x.max(1);
        let tile_count_y = tile_count_y.max(1);
        let tile_count = tile_count_x.saturating_mul(tile_count_y);
        let size_bytes = HEADER_BYTES + tile_count.saturating_mul(PER_TILE_BYTES);
        let buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("DecalClassifyBuckets"),
                size_bytes as usize,
                BufferUsage::new().with_storage().with_copy_dst(),
            )
            .into(),
        )?;

        // Header + zeroed tail (the atomic counts at each per-tile
        // offset). The `entries` slots can keep stale data — they're
        // only read up to `count`, which the classify resets to 0
        // before appending.
        let mut header_scratch = vec![0u8; size_bytes as usize];
        write_header(&mut header_scratch, tile_count_x, tile_count_y);
        Ok(Self {
            buffer,
            tile_count_x,
            tile_count_y,
            size_bytes,
            header_scratch,
        })
    }

    /// Reallocates the buffer if the viewport tile count exceeds the
    /// current allocation. Returns `true` when reallocated (caller
    /// must rebuild dependent bind groups).
    pub fn ensure_capacity(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        needed_x: u32,
        needed_y: u32,
    ) -> Result<bool, AwsmCoreError> {
        if needed_x <= self.tile_count_x && needed_y <= self.tile_count_y {
            return Ok(false);
        }
        let new_x = needed_x.max(self.tile_count_x);
        let new_y = needed_y.max(self.tile_count_y);
        *self = Self::with_tile_count(gpu, new_x, new_y)?;
        Ok(true)
    }

    /// Per-frame upload — writes the header constants + zero the
    /// per-tile atomic counts. The `entries` tail is untouched.
    pub fn reset(&self, gpu: &AwsmRendererWebGpu) -> Result<(), AwsmCoreError> {
        gpu.write_buffer(
            &self.buffer,
            None,
            self.header_scratch.as_slice(),
            None,
            None,
        )
    }
}

fn write_header(dst: &mut [u8], tile_count_x: u32, tile_count_y: u32) {
    dst[0..4].copy_from_slice(&tile_count_x.to_ne_bytes());
    dst[4..8].copy_from_slice(&tile_count_y.to_ne_bytes());
    dst[8..12].copy_from_slice(&BUCKET_CAPACITY.to_ne_bytes());
    dst[12..16].copy_from_slice(&0u32.to_ne_bytes());
    // The remaining bytes stay zero — that zeros every per-tile
    // atomic count which is what classify starts from each frame.
}
