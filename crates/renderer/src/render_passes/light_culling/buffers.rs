//! GPU storage buffers backing the light-culling pass.
//!
//! Owns three buffers:
//!
//! - **`params_buffer`** (uniform): per-frame `CullParams` reset (tiles,
//!   near/far, capacity, mesh-region offset). Re-written each frame via
//!   `writeBuffer`.
//! - **`storage_buffer`** (storage RW + copy_src): **merged** light data.
//!   Layout:
//!     `[0 .. mesh_indices_capacity_u32)`        — per-mesh CPU-written
//!                                                  light indices.
//!                                                  `MeshLightIndicesGpu`
//!                                                  writes its scratch
//!                                                  here at offset 0.
//!     `[mesh_indices_capacity_u32 .. end)`      — per-froxel GPU-written
//!                                                  stride =
//!                                                  `max_per_froxel_capacity + 1`
//!                                                  (slot 0 = atomic
//!                                                  count, slots 1.. =
//!                                                  light indices).
//!   Merging the two regions into one binding keeps the opaque pass
//!   under WebGPU's `maxStorageBuffersPerShaderStage` ceiling — the
//!   per-mesh slice and the per-froxel slice both index into the same
//!   `lights_storage` binding.
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

/// Byte size of the `CullParams` uniform — must match the WGSL struct.
/// Layout: 7 × u32 (tiles_x, tiles_y, viewport_w, viewport_h,
/// mesh_indices_capacity_u32, max_per_froxel_capacity, _pad0) + 4 × f32
/// (z_near, z_far, log_far_over_near, _pad1) — padded to 48 bytes for
/// vec4 alignment.
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

/// Storage backing for the light-culling pass.
pub struct LightCullingBuffers {
    pub params_buffer: web_sys::GpuBuffer,
    /// Merged mesh + froxel storage (see module doc).
    pub storage_buffer: web_sys::GpuBuffer,
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
    /// Number of u32 entries reserved at the head of `storage_buffer` for
    /// the per-mesh light-indices region. The cull pass writes its
    /// froxel data starting at this offset; `MeshLightIndicesGpu` writes
    /// mesh indices into the `[0..mesh_indices_capacity_u32)` prefix.
    pub mesh_indices_capacity_u32: u32,
    /// Last viewport dimensions the buffers were sized for, in pixels.
    pub viewport_w: u32,
    pub viewport_h: u32,
    /// Current `tiles_x * tiles_y * slice_count`.
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
    ) -> Result<Self, AwsmCoreError> {
        let viewport_w = viewport_w.max(1);
        let viewport_h = viewport_h.max(1);
        let mesh_indices_capacity_u32 = mesh_indices_capacity_u32.max(DEFAULT_MESH_INDICES_CAPACITY);
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
            overflow_buffer,
            overflow_readback_buffer,
            slice_count,
            max_per_froxel_capacity,
            mesh_indices_capacity_u32,
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
            // Shrink — keep buffers but track new viewport.
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
        // bytes[24..28] = _pad0, leave zero.
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
