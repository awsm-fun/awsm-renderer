//! GPU storage buffers backing the light-culling pass.
//!
//! Owns the buffers backing the light-culling pass:
//!
//! - **`params_buffer`** (uniform): per-frame `CullParams` (tiles,
//!   near/far, capacities, mesh-region offset). Re-written each frame via
//!   `writeBuffer`.
//! - **`storage_buffer`** (storage RW + copy_src): **merged** light data,
//!   laid out as (`MeshLightIndicesGpu` writes the head; the cull pass
//!   writes the tail):
//!
//!   ```text
//!   [0 .. mesh_indices_capacity_u32)    per-mesh CPU-written light indices
//!   [mesh_indices_capacity_u32 .. end)  per-froxel GPU-written slices;
//!                                       stride = max_per_froxel_capacity + 1
//!                                       (slot 0 = atomic count, 1.. = indices)
//!   ```
//!
//!   Merging the two regions into one binding keeps the opaque pass under
//!   WebGPU's `maxStorageBuffersPerShaderStage` ceiling — both slices
//!   index into the same `lights_storage` binding.
//! - **`tile_lights_buffer`** (storage RW): the two-level cull's Stage-A
//!   (`cs_tile`) output — one candidate-light slice per 2D screen tile
//!   (`tiles_x * tiles_y`), each `tile_light_capacity + 1` u32 (slot 0 =
//!   atomic count). Stage A runs the (Z-independent) side-plane test once
//!   per tile; Stage B (`cs_main`) reads each froxel's tile slice and
//!   applies only the cheap Z-slice test — so the expensive side-plane
//!   work happens once per tile instead of once per froxel (× slice_count).
//!   `tile_light_capacity` tracks the live punctual-light count (a tile
//!   can't hold more candidates than there are lights), so it never
//!   overflows and stays small for low-light scenes.
//! - **`overflow_buffer`** (storage RW + copy_src): single
//!   `atomic<u32>` incremented per dropped index. The CPU mapAsync
//!   readback drives the auto-grow path.
//!
//! Buffers are recreated when:
//! - the viewport tile count grows (ensure_viewport)
//! - the per-froxel budget grows (set_max_per_froxel_capacity)
//! - the mesh-region capacity grows (ensure_mesh_indices_capacity).

use std::sync::LazyLock;

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// Tile size in screen pixels. Mirrors the `TILE_PIXEL_SIZE` constant in
/// the cull WGSL — keep them in lockstep.
pub const TILE_PIXEL_SIZE: u32 = 16;

/// Number of view-space depth slices per screen tile.
pub const DEFAULT_SLICE_COUNT: u32 = 32;

/// Initial per-froxel light-index budget.
pub const DEFAULT_MAX_PER_FROXEL_CAPACITY: u32 = 32;

/// Initial mesh-region capacity in u32 entries. Grows 2× on overflow
/// (mirrors `MeshLightIndicesGpu`'s prior growth pattern).
pub const DEFAULT_MESH_INDICES_CAPACITY: u32 = 4;

/// Initial per-2D-screen-tile candidate-light capacity for the two-level
/// cull's Stage A (`cs_tile`) output. The capacity is **runtime**
/// (carried on `cull_params.tile_light_capacity`, written per frame) and
/// grown by [`LightCullingBuffers::ensure_tile_light_capacity`] toward
/// the live punctual-light count — a tile column can hold at most as many
/// candidates as there are punctual lights, so `live_punctual_count` is a
/// safe non-overflowing bound (no fallback path needed). Sizing it to the
/// actual light count instead of `MAX_PUNCTUAL_LIGHTS` keeps the buffer
/// small for typical low-light scenes (the common case).
pub const DEFAULT_TILE_LIGHT_CAPACITY: u32 = 16;

/// Hard ceiling for the per-tile candidate capacity — the total punctual
/// light budget. A tile can never hold more candidates than this.
pub const MAX_TILE_LIGHT_CAPACITY: u32 = crate::lights::MAX_PUNCTUAL_LIGHTS as u32;

/// Byte size of the `CullParams` uniform — must match the WGSL struct.
/// Layout: 7 u32 (tiles_x, tiles_y, viewport_w, viewport_h,
/// mesh_indices_capacity_u32, max_per_froxel_capacity, tile_light_capacity)
/// then 4 f32 (z_near, z_far, log_far_over_near, _pad1), padded to 48
/// bytes for vec4 alignment.
pub const CULL_PARAMS_BYTE_SIZE: usize = 48;

/// Byte size of a single storage-buffer entry (one `u32`).
pub const STORAGE_ENTRY_BYTE_SIZE: usize = 4;

/// Byte size of the overflow counter (single `u32`).
pub const OVERFLOW_BYTE_SIZE: usize = 4;

/// Byte size of the overflow readback staging buffer — matches
/// `OVERFLOW_BYTE_SIZE` (one `u32`). Mirrors
/// `EDGE_OVERFLOW_READBACK_BYTES` from `material_opaque::edge_buffers`.
pub const OVERFLOW_READBACK_BYTES: usize = OVERFLOW_BYTE_SIZE;

static PARAMS_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_uniform().with_copy_dst());

static STORAGE_USAGE: LazyLock<BufferUsage> = LazyLock::new(|| {
    BufferUsage::new()
        .with_storage()
        .with_copy_dst()
        .with_copy_src()
});

static OVERFLOW_USAGE: LazyLock<BufferUsage> = LazyLock::new(|| {
    BufferUsage::new()
        .with_storage()
        .with_copy_src()
        .with_copy_dst()
});

static OVERFLOW_READBACK_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_map_read().with_copy_dst());

/// Stage-A (`cs_tile`) per-2D-tile candidate list. Storage RW only —
/// `cs_tile` resets the per-tile count each frame and atomic-appends;
/// `cs_main` reads it. No host upload, so no `copy_dst`/`copy_src`.
static TILE_LIGHTS_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_storage());

/// Storage backing for the light-culling pass.
pub struct LightCullingBuffers {
    pub params_buffer: web_sys::GpuBuffer,
    /// Merged mesh + froxel storage (see module doc).
    pub storage_buffer: web_sys::GpuBuffer,
    /// Two-level cull Stage-A output: per-2D-screen-tile candidate light
    /// list (`tiles_x * tiles_y` slices, each `tile_light_capacity + 1`
    /// u32 — slot 0 = atomic count, slots 1.. = light indices). `cs_main`
    /// (Stage B) reads each froxel's tile slice and applies only the
    /// cheap Z-test, so the expensive side-plane test runs once per tile
    /// instead of once per froxel (× slice_count).
    pub tile_lights_buffer: web_sys::GpuBuffer,
    pub overflow_buffer: web_sys::GpuBuffer,
    /// Map-readable staging buffer for the per-frame `overflow_buffer`
    /// readback. The host records a `copy_buffer_to_buffer` into the
    /// command encoder after the cull dispatch, then `mapAsync`'s the
    /// staging copy to ingest `overflow_count` and call
    /// `set_max_per_froxel_capacity(current * 2)` on overflow.
    pub overflow_readback_buffer: web_sys::GpuBuffer,
    /// Number of view-space depth slices baked into the shader.
    pub slice_count: u32,
    /// Per-froxel light-index budget baked into the shader.
    pub max_per_froxel_capacity: u32,
    /// Per-2D-tile candidate-light capacity (the `tile_lights` per-tile
    /// stride is `tile_light_capacity + 1`). Runtime — written into
    /// `cull_params` and grown toward the live punctual-light count by
    /// [`Self::ensure_tile_light_capacity`]. Never exceeds
    /// `MAX_TILE_LIGHT_CAPACITY`.
    pub tile_light_capacity: u32,
    /// Number of u32 entries reserved at the head of `storage_buffer` for
    /// the per-mesh light-indices region. The cull pass writes its
    /// froxel data starting at this offset; `MeshLightIndicesGpu` writes
    /// mesh indices into the `[0..mesh_indices_capacity_u32)` prefix.
    pub mesh_indices_capacity_u32: u32,
    /// Last viewport dimensions the buffers were sized for, in pixels.
    pub viewport_w: u32,
    pub viewport_h: u32,
    /// Froxel count the buffers are currently **allocated** for — the
    /// high-watermark `tiles_x * tiles_y * slice_count` seen so far, not
    /// necessarily the count for the live viewport. On shrink
    /// (`ensure_viewport`) the buffers are deliberately kept at this
    /// larger size and `froxel_count` is left unchanged, so a later
    /// grow-back to a still-smaller-or-equal size avoids a realloc. The
    /// per-frame dispatch and `CullParams` derive the *live* froxel grid
    /// from `viewport_w/h` via `tiles_x()` / `tiles_y()` — never from
    /// this field — so use it only for allocation/realloc decisions.
    pub froxel_count: u32,
    /// Last `CullParams` payload written into `params_buffer`. Used by the
    /// per-frame upload path to skip the writeBuffer when nothing changed.
    last_params: Option<[u8; CULL_PARAMS_BYTE_SIZE]>,
    /// Reusable zero-payload for the overflow counter reset.
    zero_overflow: Vec<u8>,
}

impl LightCullingBuffers {
    /// Allocates the buffers sized for the given viewport + slice/capacity
    /// settings. The mesh region is sized to `mesh_indices_capacity_u32`
    /// u32 entries; the froxel region follows immediately after.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        viewport_w: u32,
        viewport_h: u32,
        slice_count: u32,
        max_per_froxel_capacity: u32,
        mesh_indices_capacity_u32: u32,
        tile_light_capacity: u32,
    ) -> Result<Self, AwsmCoreError> {
        let viewport_w = viewport_w.max(1);
        let viewport_h = viewport_h.max(1);
        let mesh_indices_capacity_u32 =
            mesh_indices_capacity_u32.max(DEFAULT_MESH_INDICES_CAPACITY);
        let tile_light_capacity =
            tile_light_capacity.clamp(DEFAULT_TILE_LIGHT_CAPACITY, MAX_TILE_LIGHT_CAPACITY);
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

        let stride = max_per_froxel_capacity.saturating_add(1).max(2);
        let froxel_region_entries = froxel_count.saturating_mul(stride);
        let storage_entries = mesh_indices_capacity_u32
            .saturating_add(froxel_region_entries)
            .max(1);
        let storage_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("LightCullingStorage"),
                storage_entries as usize * STORAGE_ENTRY_BYTE_SIZE,
                *STORAGE_USAGE,
            )
            .into(),
        )?;

        // Two-level cull Stage-A output: one slice per 2D screen tile
        // (independent of slice_count), `tile_light_capacity + 1` u32
        // each (slot 0 = count). `tile_light_capacity` tracks the live
        // light count (grown via `ensure_tile_light_capacity`), so the
        // buffer stays small for low-light scenes.
        let tile_count = tiles_x.saturating_mul(tiles_y).max(1);
        let tile_lights_entries = tile_count
            .saturating_mul(tile_light_capacity.saturating_add(1))
            .max(1);
        let tile_lights_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("LightCullingTileLights"),
                tile_lights_entries as usize * STORAGE_ENTRY_BYTE_SIZE,
                *TILE_LIGHTS_USAGE,
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

        let overflow_readback_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("LightCullingOverflowReadback"),
                OVERFLOW_READBACK_BYTES,
                *OVERFLOW_READBACK_USAGE,
            )
            .into(),
        )?;

        Ok(Self {
            params_buffer,
            storage_buffer,
            tile_lights_buffer,
            overflow_buffer,
            overflow_readback_buffer,
            slice_count,
            max_per_froxel_capacity,
            mesh_indices_capacity_u32,
            tile_light_capacity,
            viewport_w,
            viewport_h,
            froxel_count,
            last_params: None,
            zero_overflow: vec![0u8; OVERFLOW_BYTE_SIZE],
        })
    }

    /// Grow the buffers if the viewport tile count exceeds current
    /// capacity. Returns `true` if any buffer was recreated.
    pub fn ensure_viewport(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        viewport_w: u32,
        viewport_h: u32,
    ) -> Result<bool, AwsmCoreError> {
        let viewport_w = viewport_w.max(1);
        let viewport_h = viewport_h.max(1);
        let tiles_x_new = viewport_w.div_ceil(TILE_PIXEL_SIZE);
        let tiles_y_new = viewport_h.div_ceil(TILE_PIXEL_SIZE);
        let froxel_count_new = tiles_x_new
            .saturating_mul(tiles_y_new)
            .saturating_mul(self.slice_count)
            .max(1);
        if viewport_w == self.viewport_w
            && viewport_h == self.viewport_h
            && froxel_count_new == self.froxel_count
        {
            return Ok(false);
        }
        if froxel_count_new <= self.froxel_count
            && viewport_w == self.viewport_w
            && viewport_h == self.viewport_h
        {
            return Ok(false);
        }
        if froxel_count_new <= self.froxel_count {
            // Shrink — keep the larger buffers (high-watermark) and only
            // track the new viewport. `froxel_count` intentionally stays
            // at the allocated size so a later grow-back that still fits
            // skips reallocation; the live grid is always recomputed from
            // `viewport_w/h` (see the `froxel_count` field doc).
            self.viewport_w = viewport_w;
            self.viewport_h = viewport_h;
            return Ok(false);
        }
        *self = Self::new(
            gpu,
            viewport_w,
            viewport_h,
            self.slice_count,
            self.max_per_froxel_capacity,
            self.mesh_indices_capacity_u32,
            self.tile_light_capacity,
        )?;
        Ok(true)
    }

    /// Rebuild the buffers at a new `max_per_froxel_capacity`. The
    /// auto-grow CPU readback calls this when `overflow_counter > 0`.
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
            self.mesh_indices_capacity_u32,
            self.tile_light_capacity,
        )?;
        Ok(true)
    }

    /// Grow the mesh-indices region if `needed_capacity` exceeds current
    /// capacity. Called by `MeshLightIndicesGpu::write_gpu` when its
    /// per-frame scratch exceeds the head reserve.
    pub fn ensure_mesh_indices_capacity(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        needed_capacity_u32: u32,
    ) -> Result<bool, AwsmCoreError> {
        if needed_capacity_u32 <= self.mesh_indices_capacity_u32 {
            return Ok(false);
        }
        let new_capacity = needed_capacity_u32
            .checked_mul(2)
            .unwrap_or(needed_capacity_u32)
            .max(DEFAULT_MESH_INDICES_CAPACITY);
        *self = Self::new(
            gpu,
            self.viewport_w,
            self.viewport_h,
            self.slice_count,
            self.max_per_froxel_capacity,
            new_capacity,
            self.tile_light_capacity,
        )?;
        Ok(true)
    }

    /// Grow the per-2D-tile candidate capacity toward `needed` (the live
    /// punctual-light count) when it exceeds the current capacity. A tile
    /// column can hold at most as many candidates as there are punctual
    /// lights, so `needed = live_punctual_count` is a safe non-overflowing
    /// bound. Recreates **only** `tile_lights_buffer` (the froxel/storage
    /// buffers are independent of light count). Grows with power-of-two
    /// headroom, capped at `MAX_TILE_LIGHT_CAPACITY`. Returns `true` when
    /// recreated — the caller must then mark a bind-group recreate so the
    /// cull bind group rebinds the new buffer handle.
    pub fn ensure_tile_light_capacity(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        needed: u32,
    ) -> Result<bool, AwsmCoreError> {
        if needed <= self.tile_light_capacity {
            return Ok(false);
        }
        let new_capacity = needed
            .checked_next_power_of_two()
            .unwrap_or(MAX_TILE_LIGHT_CAPACITY)
            .clamp(DEFAULT_TILE_LIGHT_CAPACITY, MAX_TILE_LIGHT_CAPACITY);
        let tile_count = self.tiles_x().saturating_mul(self.tiles_y()).max(1);
        let entries = tile_count
            .saturating_mul(new_capacity.saturating_add(1))
            .max(1);
        self.tile_lights_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("LightCullingTileLights"),
                entries as usize * STORAGE_ENTRY_BYTE_SIZE,
                *TILE_LIGHTS_USAGE,
            )
            .into(),
        )?;
        self.tile_light_capacity = new_capacity;
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

    /// Writes the per-frame `CullParams` uniform. Cheap — skipped when
    /// the payload is unchanged.
    pub fn write_params(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        z_near: f32,
        z_far: f32,
    ) -> Result<(), AwsmCoreError> {
        let tiles_x = self.tiles_x();
        let tiles_y = self.tiles_y();
        let log_far_over_near = (z_far / z_near.max(f32::EPSILON)).ln();
        let mut bytes = [0u8; CULL_PARAMS_BYTE_SIZE];
        bytes[0..4].copy_from_slice(&tiles_x.to_ne_bytes());
        bytes[4..8].copy_from_slice(&tiles_y.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.viewport_w.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.viewport_h.to_ne_bytes());
        bytes[16..20].copy_from_slice(&self.mesh_indices_capacity_u32.to_ne_bytes());
        bytes[20..24].copy_from_slice(&self.max_per_froxel_capacity.to_ne_bytes());
        bytes[24..28].copy_from_slice(&self.tile_light_capacity.to_ne_bytes());
        bytes[28..32].copy_from_slice(&z_near.to_ne_bytes());
        bytes[32..36].copy_from_slice(&z_far.to_ne_bytes());
        bytes[36..40].copy_from_slice(&log_far_over_near.to_ne_bytes());
        // bytes[40..48] = _pad1, leave zero.

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
