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
//! Re-allocated on viewport resize via `ensure_capacity`. The header
//! is written once at allocation (it's static for the buffer's
//! lifetime). Per-frame, `reset_counts` is recorded into the command
//! encoder as a `clear_buffer` over the per-tile region only — that
//! zeros every atomic count while leaving the (mostly-stale) entries
//! tail untouched. Earlier revisions uploaded a full-buffer-sized
//! CPU scratch (~17 MB/frame at 4K); the encoder-side clear runs in
//! command order strictly before the classify dispatch reads the
//! counts and costs no CPU upload bandwidth.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    command::CommandEncoder,
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

        // Static header — never changes for this allocation; per-tile
        // counts are zeroed each frame via `reset_counts` (encoder
        // `clear_buffer`). `createBuffer` zero-initializes by default
        // so the per-tile region starts clean.
        let mut header_bytes = [0u8; HEADER_BYTES as usize];
        write_header(&mut header_bytes, tile_count_x, tile_count_y);
        gpu.write_buffer(&buffer, None, header_bytes.as_slice(), None, None)?;

        Ok(Self {
            buffer,
            tile_count_x,
            tile_count_y,
            size_bytes,
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

    /// Per-frame reset of the atomic counts. Recorded into the command
    /// encoder so it runs in command order strictly before the
    /// classify dispatch reads the counts and writes via atomicAdd.
    /// Zeros the entire per-tile region (counts + entries); the
    /// entries are only read up to `count`, so wiping them too is
    /// harmless and saves having to walk per-tile offsets with a
    /// stride-aware clear (the WebGPU `clearBuffer` API only supports
    /// contiguous ranges).
    pub fn reset_counts(&self, encoder: &CommandEncoder) {
        encoder.clear_buffer(
            &self.buffer,
            Some(HEADER_BYTES),
            Some(self.size_bytes.saturating_sub(HEADER_BYTES)),
        );
    }
}

fn write_header(dst: &mut [u8], tile_count_x: u32, tile_count_y: u32) {
    dst[0..4].copy_from_slice(&tile_count_x.to_ne_bytes());
    dst[4..8].copy_from_slice(&tile_count_y.to_ne_bytes());
    dst[8..12].copy_from_slice(&BUCKET_CAPACITY.to_ne_bytes());
    dst[12..16].copy_from_slice(&0u32.to_ne_bytes());
}
