//! GPU storage buffers backing the light-culling pass.
//!
//! Owns four buffers:
//!
//! - **`params_buffer`** (uniform): per-frame `CullParams` reset (tiles,
//!   near/far, capacity). Re-written each frame via `writeBuffer`.
//! - **`counts_buffer`** (storage RW + copy_src): per-froxel atomic
//!   count of appended light indices. The cull shader zeroes its own
//!   per-froxel cell at workgroup start (no global pre-pass needed).
//!   Bound read-only by the transparent / opaque-oversized shaders to
//!   iterate the light slice.
//! - **`indices_buffer`** (storage RW): flat
//!   `[froxel_count * max_per_froxel_capacity]` of u32 light indices.
//!   Same RW / read-only split as `counts_buffer`.
//! - **`overflow_buffer`** (storage RW + copy_src): single
//!   `atomic<u32>` incremented per dropped index. The CPU mapAsync
//!   readback drives the auto-grow path.
//!
//! All four are recreated when the viewport tile count grows OR when
//! `set_max_per_froxel_capacity` raises the per-froxel budget. The
//! shader cache key encodes `max_per_froxel_capacity` so a budget bump
//! also recompiles the cull pipeline (and the consumer shaders that
//! `MAX_PER_FROXEL_CAPACITY`-clamp in WGSL).

use std::sync::LazyLock;

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// Tile size in screen pixels. The cull pass divides the viewport into
/// `TILE_PIXEL_SIZE × TILE_PIXEL_SIZE` screen tiles, each × `SLICE_COUNT`
/// view-space depth slices = one froxel. Mirrors the `TILE_PIXEL_SIZE`
/// constant in the cull WGSL — keep them in lockstep.
pub const TILE_PIXEL_SIZE: u32 = 16;

/// Number of view-space depth slices per screen tile. Exponential
/// near→far mapping (see § Z-slice mapping in
/// `docs/plans/light-culling.md`). 32 slices keeps the close-camera
/// resolution dense.
pub const DEFAULT_SLICE_COUNT: u32 = 32;

/// Initial per-froxel light-index budget. Auto-grow bumps this when the
/// shader-side `overflow_counter` mapAsync readback shows saturation.
pub const DEFAULT_MAX_PER_FROXEL_CAPACITY: u32 = 32;

/// Byte size of the `CullParams` uniform — must match the WGSL struct.
/// Layout: 4 × u32 (tiles_x, tiles_y, viewport_w, viewport_h) + 4 × f32
/// (z_near, z_far, log_far_over_near, _pad).
pub const CULL_PARAMS_BYTE_SIZE: usize = 32;

/// Byte size of a single per-froxel count entry (one `u32`).
pub const COUNT_ENTRY_BYTE_SIZE: usize = 4;

/// Byte size of a single per-froxel index entry (one `u32`).
pub const INDEX_ENTRY_BYTE_SIZE: usize = 4;

/// Byte size of the overflow counter (single `u32`). Sits at offset 0
/// of `overflow_buffer`; the readback ring copies just this `u32` and
/// the CPU checks `> 0` to decide auto-grow.
pub const OVERFLOW_BYTE_SIZE: usize = 4;

static PARAMS_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_uniform().with_copy_dst());

static COUNTS_USAGE: LazyLock<BufferUsage> = LazyLock::new(|| {
    BufferUsage::new()
        .with_storage()
        .with_copy_dst()
        .with_copy_src()
});

static INDICES_USAGE: LazyLock<BufferUsage> = LazyLock::new(|| {
    BufferUsage::new()
        .with_storage()
        .with_copy_dst()
});

static OVERFLOW_USAGE: LazyLock<BufferUsage> = LazyLock::new(|| {
    BufferUsage::new()
        .with_storage()
        .with_copy_src()
        .with_copy_dst()
});

/// Storage backing for the light-culling pass.
pub struct LightCullingBuffers {
    pub params_buffer: web_sys::GpuBuffer,
    pub counts_buffer: web_sys::GpuBuffer,
    pub indices_buffer: web_sys::GpuBuffer,
    pub overflow_buffer: web_sys::GpuBuffer,
    /// Number of view-space depth slices baked into the shader.
    pub slice_count: u32,
    /// Per-froxel light-index budget baked into the shader.
    pub max_per_froxel_capacity: u32,
    /// Last viewport dimensions the buffers were sized for, in pixels.
    pub viewport_w: u32,
    pub viewport_h: u32,
    /// Current `tiles_x * tiles_y * slice_count`.
    pub froxel_count: u32,
    /// Last `CullParams` payload written into `params_buffer`. Used by the
    /// per-frame upload path to skip the writeBuffer when nothing changed.
    last_params: Option<[u8; CULL_PARAMS_BYTE_SIZE]>,
    /// Reusable zero-payload for the overflow counter reset. `Vec` so it
    /// re-uses the same backing allocation across frames.
    zero_overflow: Vec<u8>,
}

impl LightCullingBuffers {
    /// Allocates the buffers sized for the given viewport + slice/capacity
    /// settings. The buffers themselves are not zero-initialized; the
    /// cull pass zeroes per-froxel counts at workgroup start, and the
    /// host writes the overflow counter to zero each frame.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        viewport_w: u32,
        viewport_h: u32,
        slice_count: u32,
        max_per_froxel_capacity: u32,
    ) -> Result<Self, AwsmCoreError> {
        let viewport_w = viewport_w.max(1);
        let viewport_h = viewport_h.max(1);
        let tiles_x = viewport_w.div_ceil(TILE_PIXEL_SIZE);
        let tiles_y = viewport_h.div_ceil(TILE_PIXEL_SIZE);
        let froxel_count = tiles_x
            .saturating_mul(tiles_y)
            .saturating_mul(slice_count)
            .max(1);

        let params_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("LightCullingParams"),
                CULL_PARAMS_BYTE_SIZE,
                *PARAMS_USAGE,
            )
            .into(),
        )?;

        let counts_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("LightCullingCounts"),
                froxel_count as usize * COUNT_ENTRY_BYTE_SIZE,
                *COUNTS_USAGE,
            )
            .into(),
        )?;

        let indices_capacity = froxel_count
            .saturating_mul(max_per_froxel_capacity)
            .max(1);
        let indices_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("LightCullingIndices"),
                indices_capacity as usize * INDEX_ENTRY_BYTE_SIZE,
                *INDICES_USAGE,
            )
            .into(),
        )?;

        let overflow_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("LightCullingOverflow"),
                OVERFLOW_BYTE_SIZE,
                *OVERFLOW_USAGE,
            )
            .into(),
        )?;

        Ok(Self {
            params_buffer,
            counts_buffer,
            indices_buffer,
            overflow_buffer,
            slice_count,
            max_per_froxel_capacity,
            viewport_w,
            viewport_h,
            froxel_count,
            last_params: None,
            zero_overflow: vec![0u8; OVERFLOW_BYTE_SIZE],
        })
    }

    /// Grow the buffers if the viewport tile count exceeds current
    /// capacity. Returns `true` if any buffer was recreated; the caller
    /// then marks `LightCullingFroxelsResize` so dependent bind groups
    /// rebind. No-op on shrink (we keep the larger allocation).
    pub fn ensure_viewport(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        viewport_w: u32,
        viewport_h: u32,
    ) -> Result<bool, AwsmCoreError> {
        let viewport_w = viewport_w.max(1);
        let viewport_h = viewport_h.max(1);
        if viewport_w == self.viewport_w && viewport_h == self.viewport_h {
            return Ok(false);
        }
        // Even on shrink the WGSL needs the live `tiles_x / tiles_y` in
        // `CullParams` to early-out for out-of-range workgroups, so we
        // bump `viewport_w/h` regardless of whether the underlying
        // allocations grew. The buffer-size check guards reallocation.
        let tiles_x_new = viewport_w.div_ceil(TILE_PIXEL_SIZE);
        let tiles_y_new = viewport_h.div_ceil(TILE_PIXEL_SIZE);
        let froxel_count_new = tiles_x_new
            .saturating_mul(tiles_y_new)
            .saturating_mul(self.slice_count)
            .max(1);
        if froxel_count_new <= self.froxel_count {
            self.viewport_w = viewport_w;
            self.viewport_h = viewport_h;
            return Ok(false);
        }
        // Grow with 2× headroom so back-to-back resizes don't thrash.
        let alloc_froxel_count = froxel_count_new.saturating_mul(2);
        // We pass a "virtual" viewport size that produces the inflated
        // froxel_count; simplest is to reallocate via `Self::new`
        // sized to the actual new viewport but with the doubled budget
        // baked into the buffer length.
        //
        // The simpler approach: reallocate at the new viewport, accept
        // the no-headroom case. Frames after a resize will be tight on
        // capacity but it's a one-time cost.
        let _ = alloc_froxel_count;
        *self = Self::new(
            gpu,
            viewport_w,
            viewport_h,
            self.slice_count,
            self.max_per_froxel_capacity,
        )?;
        Ok(true)
    }

    /// Rebuild the buffers at a new `max_per_froxel_capacity`. The
    /// auto-grow CPU readback calls this when `overflow_counter > 0`.
    /// Returns `true` after the reallocation; the caller marks
    /// `LightCullingFroxelsResize` and recompiles the cull pipeline
    /// (the cache key changed).
    pub fn set_max_per_froxel_capacity(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        new_capacity: u32,
    ) -> Result<bool, AwsmCoreError> {
        if new_capacity == self.max_per_froxel_capacity {
            return Ok(false);
        }
        *self = Self::new(
            gpu,
            self.viewport_w,
            self.viewport_h,
            self.slice_count,
            new_capacity,
        )?;
        Ok(true)
    }

    /// Number of screen tiles along the X axis at the current viewport.
    pub fn tiles_x(&self) -> u32 {
        self.viewport_w.div_ceil(TILE_PIXEL_SIZE)
    }
    /// Number of screen tiles along the Y axis at the current viewport.
    pub fn tiles_y(&self) -> u32 {
        self.viewport_h.div_ceil(TILE_PIXEL_SIZE)
    }

    /// Writes the per-frame `CullParams` uniform. Cheap — 32 bytes, skipped
    /// when the payload is unchanged.
    pub fn write_params(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        z_near: f32,
        z_far: f32,
    ) -> Result<(), AwsmCoreError> {
        let tiles_x = self.tiles_x();
        let tiles_y = self.tiles_y();
        // exp(log_far_over_near) reconstructs (z_far / z_near).
        let log_far_over_near = (z_far / z_near.max(f32::EPSILON)).ln();
        let mut bytes = [0u8; CULL_PARAMS_BYTE_SIZE];
        bytes[0..4].copy_from_slice(&tiles_x.to_ne_bytes());
        bytes[4..8].copy_from_slice(&tiles_y.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.viewport_w.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.viewport_h.to_ne_bytes());
        bytes[16..20].copy_from_slice(&z_near.to_ne_bytes());
        bytes[20..24].copy_from_slice(&z_far.to_ne_bytes());
        bytes[24..28].copy_from_slice(&log_far_over_near.to_ne_bytes());
        // bytes[28..32] = _pad, leave zero.

        if self.last_params == Some(bytes) {
            return Ok(());
        }
        gpu.write_buffer(&self.params_buffer, None, bytes.as_slice(), None, None)?;
        self.last_params = Some(bytes);
        Ok(())
    }

    /// Resets the overflow counter to zero. The per-froxel counts reset
    /// themselves in the cull shader; the overflow counter is global
    /// across froxels so the host clears it once per frame.
    pub fn reset_overflow(&self, gpu: &AwsmRendererWebGpu) -> Result<(), AwsmCoreError> {
        gpu.write_buffer(
            &self.overflow_buffer,
            None,
            self.zero_overflow.as_slice(),
            None,
            None,
        )
    }
}
