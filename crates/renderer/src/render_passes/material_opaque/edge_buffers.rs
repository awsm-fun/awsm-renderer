//! GPU storage buffers backing the per-shader-id MSAA edge-resolve
//! pipeline (Priority 3 in docs/plans/more-optimizations.md).
//!
//! The classify pass extension allocates a compact `edge_pixel_id` per
//! edge pixel (via an atomic counter capped at [`MAX_EDGE_BUDGET`]),
//! writes its `(x, y)` coords into [`MaterialEdgeBuffers::edge_to_xy`],
//! its 4-byte shader_id slot map into
//! [`MaterialEdgeBuffers::edge_slot_map`], and a per-shader-id
//! `(edge_pixel_id, sample_mask_byte)` entry into the matching
//! per-shader-id sample-list bucket.
//!
//! The per-shader-id `material_edge_resolve_{shader_id}` pipelines
//! indirect-dispatch over their bucket's sample list, shade each
//! sample, and write the summed `(color, sample_count)` into
//! [`MaterialEdgeBuffers::accumulator`] at
//! `edge_pixel_id × 4 + slot_index`. The `final_blend` pipeline
//! indirect-dispatches one thread per edge_pixel_id, reads the 4
//! accumulator slots, blends weighted by their sample counts, and
//! writes the result to `opaque_tex[edge_to_xy[edge_pixel_id]]`.
//!
//! See [§ Pass structure](docs/plans/more-optimizations.md#pass-structure)
//! and [§ Memory budget](docs/plans/more-optimizations.md#memory-budget)
//! for the architectural design.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// Default per-shader-id sample-list capacity in entries (each entry is
/// 4 bytes: `(edge_pixel_id:24, sample_mask:8)`). At 4 entries per pixel
/// worst-case (one per MSAA sample), 4 × MAX_EDGE_BUDGET would be the
/// pathological upper bound; in practice ~1.5 × MAX_EDGE_BUDGET total
/// across all buckets is plenty.
///
/// Per-bucket capacity is computed as `MAX_EDGE_BUDGET × 2` so even a
/// single shader_id owning every edge fits without saturating.
pub const SAMPLE_ENTRIES_PER_BUCKET_MULTIPLIER: u32 = 2;

/// Maximum edge_pixel_id allocated by classify before the overflow tail
/// kicks in. Sized for desktop targets; mobile profiles override via
/// [`MaterialEdgeBuffers::new_with_budget`].
///
/// At 512k entries: ~37 MB of accumulator. Atomic counter saturates at
/// this value; subsequent edge pixels fall through the overflow fast
/// path (a tiny atomic-add to a fixed-point reserved accumulator
/// region — slow path safety net per the plan doc).
pub const DEFAULT_MAX_EDGE_BUDGET_DESKTOP: u32 = 512 * 1024;

/// Mobile-profile default (smaller accumulator footprint).
pub const DEFAULT_MAX_EDGE_BUDGET_MOBILE: u32 = 256 * 1024;

/// Bytes per indirect-args entry (matches the classify pass's
/// `INDIRECT_ARGS_STRIDE`): `(x: u32, y: u32, z: u32, _pad: u32)`.
pub const INDIRECT_ARGS_STRIDE: u32 = 16;

/// Single u32 packing both the edge pixel ID (low 24 bits) and the
/// sample mask (high 8 bits) into one storage word.
///
/// `edge_pixel_id` lives in bits [0, 23] (24-bit IDs ⇒ MAX_EDGE_BUDGET ≤ 16M).
/// `sample_mask` lives in bits [24, 31] — one bit per MSAA sample (max 4
/// samples today, 8 reserved for future expansion).
#[inline]
pub fn pack_edge_sample_entry(edge_pixel_id: u32, sample_mask: u8) -> u32 {
    (edge_pixel_id & 0x00FF_FFFF) | ((sample_mask as u32) << 24)
}

/// Inverse of [`pack_edge_sample_entry`].
#[inline]
pub fn unpack_edge_sample_entry(packed: u32) -> (u32, u8) {
    let edge_pixel_id = packed & 0x00FF_FFFF;
    let sample_mask = ((packed >> 24) & 0xFF) as u8;
    (edge_pixel_id, sample_mask)
}

/// Packed `(x: u16, y: u16)` for [`MaterialEdgeBuffers::edge_to_xy`].
///
/// 16 bits per axis caps us at 65535-pixel viewports per axis (plenty
/// for any near-term display surface).
#[inline]
pub fn pack_xy(x: u32, y: u32) -> u32 {
    (x & 0xFFFF) | ((y & 0xFFFF) << 16)
}

/// Inverse of [`pack_xy`].
#[inline]
pub fn unpack_xy(packed: u32) -> (u32, u32) {
    let x = packed & 0xFFFF;
    let y = (packed >> 16) & 0xFFFF;
    (x, y)
}

/// Number of accumulator slots per edge pixel — at most 4 distinct
/// shader_ids can contribute samples at a single edge pixel (one per
/// MSAA sample), so 4 slots per edge is exact.
pub const ACCUMULATOR_SLOTS_PER_EDGE: u32 = 4;

/// Bytes per accumulator slot (vec4<f32>).
pub const ACCUMULATOR_SLOT_BYTES: u32 = 16;

/// Header byte layout for the edge counters + indirect-args region.
///
/// Layout (16-byte aligned):
///   - `edge_count: atomic<u32>`             — bytes [0, 4)
///   - `edge_overflow_count: atomic<u32>`    — bytes [4, 8)
///   - `final_blend_args: DispatchIndirectArgs` — bytes [16, 32) (aligned)
///   - `skybox_edge_args: DispatchIndirectArgs` — bytes [32, 48)
///   - `per_shader_id_args: array<DispatchIndirectArgs, bucket_count>` — bytes [48, 48 + bucket_count*16)
///
/// Buckets line up with `dynamic_materials::bucket_entries()` (first-party
/// + dynamic) — same indexing scheme as the classify pass uses.
pub fn header_bytes(bucket_count: u32) -> u32 {
    // 2 atomic u32 counters + 8 bytes pad to 16-byte align.
    let counters_bytes = 16u32;
    let final_blend_args_bytes = INDIRECT_ARGS_STRIDE;
    let skybox_edge_args_bytes = INDIRECT_ARGS_STRIDE;
    let per_shader_args_bytes = bucket_count.saturating_mul(INDIRECT_ARGS_STRIDE);
    let unpadded =
        counters_bytes + final_blend_args_bytes + skybox_edge_args_bytes + per_shader_args_bytes;
    // Pad to 16 to keep the trailing arrays aligned.
    (unpadded + 15) & !15
}

/// Computed offset of the `edge_to_xy` array (in bytes) inside the
/// composite buffer. Comes right after the header.
pub fn edge_to_xy_offset(bucket_count: u32) -> u32 {
    header_bytes(bucket_count)
}

/// Computed offset of the `edge_slot_map` array (in bytes).
pub fn edge_slot_map_offset(bucket_count: u32, max_edge_budget: u32) -> u32 {
    edge_to_xy_offset(bucket_count) + max_edge_budget.saturating_mul(4)
}

/// Computed offset of the `accumulator` array (in bytes).
pub fn accumulator_offset(bucket_count: u32, max_edge_budget: u32) -> u32 {
    edge_slot_map_offset(bucket_count, max_edge_budget) + max_edge_budget.saturating_mul(4)
}

/// Total size of the accumulator array, in bytes.
pub fn accumulator_bytes(max_edge_budget: u32) -> u32 {
    max_edge_budget
        .saturating_mul(ACCUMULATOR_SLOTS_PER_EDGE)
        .saturating_mul(ACCUMULATOR_SLOT_BYTES)
}

/// Computed offset of the per-shader-id sample entries region.
pub fn sample_entries_offset(bucket_count: u32, max_edge_budget: u32) -> u32 {
    accumulator_offset(bucket_count, max_edge_budget) + accumulator_bytes(max_edge_budget)
}

/// Per-bucket sample-list capacity (in entries; each entry 4 bytes).
pub fn sample_entries_per_bucket(max_edge_budget: u32) -> u32 {
    max_edge_budget.saturating_mul(SAMPLE_ENTRIES_PER_BUCKET_MULTIPLIER)
}

/// Total bytes for the entire composite edge buffer (header + per-edge
/// arrays + per-shader-id sample lists).
pub fn total_buffer_bytes(bucket_count: u32, max_edge_budget: u32) -> u32 {
    let sample_entries_base = sample_entries_offset(bucket_count, max_edge_budget);
    let per_bucket_bytes = sample_entries_per_bucket(max_edge_budget).saturating_mul(4);
    let all_buckets_bytes = bucket_count.saturating_mul(per_bucket_bytes);
    sample_entries_base + all_buckets_bytes
}

/// Composite GPU buffer for the MSAA edge-resolve flow.
///
/// One buffer holds the header (counters + indirect args), the
/// per-edge arrays (`edge_to_xy`, `edge_slot_map`, `accumulator`), and
/// the per-shader-id sample-entry lists. Resized when the bucket count
/// (a dynamic-material registration grew the registry) or the
/// max_edge_budget changes.
pub struct MaterialEdgeBuffers {
    /// Storage + indirect-arg GPU buffer. Used by classify
    /// (read-write atomics), by per-shader-id edge_resolve pipelines
    /// (read-only sample lists + write-only accumulator slot per
    /// thread), and by final_blend (read-only accumulator + read-only
    /// edge_to_xy).
    pub buffer: web_sys::GpuBuffer,
    pub bucket_count: u32,
    pub max_edge_budget: u32,
    pub size_bytes: u32,
    /// CPU staging vec sized to `header_bytes(bucket_count)`. Written
    /// once per frame at the top of classify to clear the atomic
    /// counters + reset the indirect-arg `(x=0, y=1, z=1, pad=0)`
    /// slots. Tile arrays are overwritten in place by the shader.
    header_scratch: Vec<u8>,
}

impl MaterialEdgeBuffers {
    /// Creates the edge buffers with the default desktop-profile
    /// budget. Use [`Self::new_with_budget`] for explicit control.
    pub fn new(gpu: &AwsmRendererWebGpu, bucket_count: u32) -> Result<Self, AwsmCoreError> {
        Self::new_with_budget(gpu, bucket_count, DEFAULT_MAX_EDGE_BUDGET_DESKTOP)
    }

    /// Creates the edge buffers with an explicit budget. The runtime
    /// platform-detect should pick between
    /// [`DEFAULT_MAX_EDGE_BUDGET_DESKTOP`] and
    /// [`DEFAULT_MAX_EDGE_BUDGET_MOBILE`] (or smaller on
    /// memory-constrained targets).
    pub fn new_with_budget(
        gpu: &AwsmRendererWebGpu,
        bucket_count: u32,
        max_edge_budget: u32,
    ) -> Result<Self, AwsmCoreError> {
        let bucket_count = bucket_count.max(1);
        let max_edge_budget = max_edge_budget.max(1);
        let size_bytes = total_buffer_bytes(bucket_count, max_edge_budget);

        let buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("MaterialEdgeBuffers"),
                size_bytes as usize,
                BufferUsage::new()
                    .with_storage()
                    .with_indirect()
                    .with_copy_dst(),
            )
            .into(),
        )?;

        let header = header_bytes(bucket_count) as usize;
        let mut header_scratch = vec![0u8; header];
        write_header(&mut header_scratch, bucket_count);

        Ok(Self {
            buffer,
            bucket_count,
            max_edge_budget,
            size_bytes,
            header_scratch,
        })
    }

    /// Recreates the buffer if a dynamic-material registration grew
    /// the bucket count past the current size. Caller is responsible
    /// for rebuilding dependent bind groups when this returns `true`.
    pub fn ensure_bucket_count(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        needed_bucket_count: u32,
    ) -> Result<bool, AwsmCoreError> {
        if needed_bucket_count <= self.bucket_count {
            return Ok(false);
        }
        *self = Self::new_with_budget(gpu, needed_bucket_count, self.max_edge_budget)?;
        Ok(true)
    }

    /// Writes the per-frame header reset into the buffer: zeroes the
    /// edge_count + edge_overflow_count atomics and re-asserts
    /// `(y=1, z=1)` on every indirect-arg slot. Tile arrays remain
    /// untouched (overwritten by the shader).
    pub fn reset_header(&self, gpu: &AwsmRendererWebGpu) -> Result<(), AwsmCoreError> {
        gpu.write_buffer(&self.buffer, None, self.header_scratch.as_slice(), None, None)
    }

    /// Byte offset of the final_blend indirect-arg slot. Passed to
    /// `dispatchWorkgroupsIndirect` for the final blend pipeline.
    pub fn final_blend_args_offset() -> u32 {
        // After the two atomic u32 counters + 8 bytes alignment pad.
        16
    }

    /// Byte offset of the skybox_edge indirect-arg slot.
    pub fn skybox_edge_args_offset() -> u32 {
        16 + INDIRECT_ARGS_STRIDE
    }

    /// Byte offset of the per-shader-id indirect-arg slot for bucket
    /// `bucket_index`. Passed to `dispatchWorkgroupsIndirect`.
    pub fn per_shader_args_offset(bucket_index: u32) -> u32 {
        16 + 2 * INDIRECT_ARGS_STRIDE + bucket_index * INDIRECT_ARGS_STRIDE
    }
}

/// Writes the initial header into `dst`. Layout per the module-level
/// docs: 2 atomic counters + 1 final_blend args slot + 1 skybox_edge
/// args slot + bucket_count per-shader-id args slots.
pub fn write_header(dst: &mut [u8], bucket_count: u32) {
    let one = 1u32.to_ne_bytes();
    // Counters: both zero (default).
    // (bytes [0, 4) and [4, 8) are already zeroed by vec![0u8; ...].)

    // 8-byte alignment pad: zeros.

    // final_blend args slot at byte offset 16.
    let final_blend_base = 16usize;
    dst[final_blend_base..final_blend_base + 4].copy_from_slice(&[0; 4]); // x
    dst[final_blend_base + 4..final_blend_base + 8].copy_from_slice(&one); // y
    dst[final_blend_base + 8..final_blend_base + 12].copy_from_slice(&one); // z
    dst[final_blend_base + 12..final_blend_base + 16].copy_from_slice(&[0; 4]); // pad

    // skybox_edge args slot at byte offset 32.
    let skybox_base = 16 + INDIRECT_ARGS_STRIDE as usize;
    dst[skybox_base..skybox_base + 4].copy_from_slice(&[0; 4]); // x
    dst[skybox_base + 4..skybox_base + 8].copy_from_slice(&one); // y
    dst[skybox_base + 8..skybox_base + 12].copy_from_slice(&one); // z
    dst[skybox_base + 12..skybox_base + 16].copy_from_slice(&[0; 4]); // pad

    // Per-shader-id args slots.
    let per_shader_base = 16 + 2 * INDIRECT_ARGS_STRIDE as usize;
    for bucket in 0..bucket_count as usize {
        let base = per_shader_base + bucket * INDIRECT_ARGS_STRIDE as usize;
        dst[base..base + 4].copy_from_slice(&[0; 4]); // x
        dst[base + 4..base + 8].copy_from_slice(&one); // y
        dst[base + 8..base + 12].copy_from_slice(&one); // z
        dst[base + 12..base + 16].copy_from_slice(&[0; 4]); // pad
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_round_trip_xy() {
        for (x, y) in [(0u32, 0u32), (123, 456), (65535, 65535), (1, 2)] {
            let packed = pack_xy(x, y);
            let (rx, ry) = unpack_xy(packed);
            assert_eq!((rx, ry), (x, y));
        }
    }

    #[test]
    fn pack_round_trip_entry() {
        for (id, mask) in [(0u32, 0u8), (12345, 0b1010), (0x00FF_FFFF, 0xFF)] {
            let packed = pack_edge_sample_entry(id, mask);
            let (rid, rmask) = unpack_edge_sample_entry(packed);
            assert_eq!((rid, rmask), (id, mask));
        }
    }

    #[test]
    fn header_size_is_aligned() {
        for bucket_count in [1u32, 4, 5, 17] {
            assert_eq!(header_bytes(bucket_count) % 16, 0);
        }
    }
}
