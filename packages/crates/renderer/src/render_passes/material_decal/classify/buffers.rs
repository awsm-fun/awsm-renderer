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
//! Re-allocated when the viewport's tile count outgrows the allocation
//! (`ensure_capacity`). The header carries the CURRENT tile grid: it is
//! rewritten whenever the grid changes, also when the new grid fits the
//! allocation (a smaller viewport, or TAA's render scale below 1): the
//! classify pass maps screen UV onto `tile_count_x × 8` pixels and the
//! shading pass indexes tiles with `tile_count_x` as the row stride, so a
//! stale, larger grid would put each decal's tiles in the wrong place.
//! Per-frame, `reset_counts` is recorded into the command
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
    /// The current tile grid (what the header says).
    pub tile_count_x: u32,
    pub tile_count_y: u32,
    /// Tiles the allocation holds (>= `tile_count_x * tile_count_y`).
    pub capacity_tiles: u32,
    pub size_bytes: u32,
}

/// What [`DecalClassifyBuffers::ensure_capacity`] does for a tile grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GridChange {
    /// Same grid: nothing to do.
    Unchanged,
    /// A different grid that fits the allocation: rewrite the header only.
    RewriteHeader,
    /// More tiles than the allocation holds: a new buffer.
    Reallocate,
}

fn grid_change(current: (u32, u32), capacity_tiles: u32, needed: (u32, u32)) -> GridChange {
    let needed = (needed.0.max(1), needed.1.max(1));
    if needed == current {
        GridChange::Unchanged
    } else if needed.0.saturating_mul(needed.1) <= capacity_tiles {
        GridChange::RewriteHeader
    } else {
        GridChange::Reallocate
    }
}

impl DecalClassifyBuffers {
    /// Creates a buffer sized for a 1×1 tile grid; `ensure_capacity`
    /// resizes on the first frame against the real viewport.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self, AwsmCoreError> {
        Self::with_tile_count(gpu, 1, 1, 1)
    }

    fn with_tile_count(
        gpu: &AwsmRendererWebGpu,
        tile_count_x: u32,
        tile_count_y: u32,
        capacity_tiles: u32,
    ) -> Result<Self, AwsmCoreError> {
        let tile_count_x = tile_count_x.max(1);
        let tile_count_y = tile_count_y.max(1);
        let capacity_tiles = capacity_tiles.max(tile_count_x.saturating_mul(tile_count_y));
        let size_bytes = HEADER_BYTES + capacity_tiles.saturating_mul(PER_TILE_BYTES);
        let buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("DecalClassifyBuckets"),
                size_bytes as usize,
                BufferUsage::new().with_storage().with_copy_dst(),
            )
            .into(),
        )?;

        // Header = the current grid (rewritten by `ensure_capacity` when the
        // grid changes); per-tile counts are zeroed each frame via
        // `reset_counts` (encoder `clear_buffer`). `createBuffer`
        // zero-initializes by default so the per-tile region starts clean.
        let this = Self {
            buffer,
            tile_count_x,
            tile_count_y,
            capacity_tiles,
            size_bytes,
        };
        this.write_grid(gpu)?;
        Ok(this)
    }

    fn write_grid(&self, gpu: &AwsmRendererWebGpu) -> Result<(), AwsmCoreError> {
        let mut header_bytes = [0u8; HEADER_BYTES as usize];
        write_header(&mut header_bytes, self.tile_count_x, self.tile_count_y);
        gpu.write_buffer(&self.buffer, None, header_bytes.as_slice(), None, None)
    }

    /// Makes the header describe the viewport's tile grid, reallocating
    /// when it holds more tiles than the allocation. Returns `true` when
    /// reallocated (caller must rebuild dependent bind groups).
    pub fn ensure_capacity(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        needed_x: u32,
        needed_y: u32,
    ) -> Result<bool, AwsmCoreError> {
        match grid_change(
            (self.tile_count_x, self.tile_count_y),
            self.capacity_tiles,
            (needed_x, needed_y),
        ) {
            GridChange::Unchanged => Ok(false),
            GridChange::RewriteHeader => {
                self.tile_count_x = needed_x.max(1);
                self.tile_count_y = needed_y.max(1);
                self.write_grid(gpu)?;
                Ok(false)
            }
            GridChange::Reallocate => {
                *self = Self::with_tile_count(gpu, needed_x, needed_y, self.capacity_tiles)?;
                Ok(true)
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A smaller grid (window shrink, TAA render scale < 1) must reach the
    /// header even though it fits the allocation: the old code only acted
    /// on growth, so the shaders kept the larger grid and decals were
    /// drawn on the wrong tiles (cut off at TAA 0.67).
    #[test]
    fn a_smaller_grid_rewrites_the_header_without_reallocating() {
        let native = (240, 180); // 1920x1440 / 8
        let cap = native.0 * native.1;
        let scaled = (161, 121); // 0.67
        assert_eq!(grid_change(native, cap, scaled), GridChange::RewriteHeader);
        assert_eq!(grid_change(scaled, cap, native), GridChange::RewriteHeader);
        assert_eq!(grid_change(native, cap, native), GridChange::Unchanged);
    }

    #[test]
    fn a_grid_with_more_tiles_than_the_allocation_reallocates() {
        assert_eq!(grid_change((1, 1), 1, (240, 180)), GridChange::Reallocate);
        // Wider but shorter: fewer tiles in total still fits.
        assert_eq!(
            grid_change((100, 100), 10_000, (200, 50)),
            GridChange::RewriteHeader
        );
        assert_eq!(
            grid_change((100, 100), 10_000, (200, 51)),
            GridChange::Reallocate
        );
        // A zero-size viewport keeps a 1x1 grid.
        assert_eq!(grid_change((1, 1), 1, (0, 0)), GridChange::Unchanged);
    }
}
