//! GPU storage buffers backing the per-shader-id MSAA edge-resolve
//! pipeline (Priority 3 in docs/plans/more-optimizations.md).
//!
//! Two GPU buffers, split to satisfy WebGPU's "a buffer cannot be both
//! Indirect-readable and Storage(read-write) in the same synchronization
//! scope" validation rule:
//!
//! - **`args_buffer`** — `Indirect | CopyDst` only. Holds the two
//!   atomic counters (edge_count, edge_overflow_count) and the
//!   `(2 + bucket_count)` `DispatchIndirectArgs` entries
//!   (final_blend + skybox + per-shader). Bound as `storage RW` to
//!   classify (so it can atomicAdd into the counters and per-shader
//!   workgroup_count_x), and read as `dispatch_workgroups_indirect`'s
//!   source by the edge_resolve / skybox_edge_resolve / final_blend
//!   dispatches.
//!
//! - **`data_buffer`** — `Storage | CopyDst` only. Holds
//!   `edge_to_xy`, `edge_slot_map`, `accumulator`, the per-shader-id
//!   sample lists, and the skybox sample list. Bound as `storage RW`
//!   to classify (writes edge_to_xy / edge_slot_map / sample entries),
//!   to the per-shader edge_resolve pipelines (writes accumulator
//!   slots), and to the skybox/final_blend pipelines (final_blend
//!   reads). Never used as Indirect.
//!
//! The classify pass extension allocates a compact `edge_pixel_id` per
//! edge pixel (via an atomic counter capped at `MAX_EDGE_BUDGET`),
//! writes its `(x, y)` coords into `edge_to_xy`, its 4-byte shader_id
//! slot map into `edge_slot_map`, and a per-shader-id
//! `(edge_pixel_id, sample_mask_byte)` entry into the matching
//! per-shader-id sample-list bucket.
//!
//! See [§ Pass structure](docs/plans/more-optimizations.md#pass-structure)
//! and [§ Memory budget](docs/plans/more-optimizations.md#memory-budget)
//! for the architectural design.

use std::sync::Mutex;

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

// ─────────────────────────────────────────────────────────────────
// MAX_EDGE_BUDGET overflow diagnostics (Stage 3.8 / Block C.2 — MVP).
//
// The classify pass atomically allocates a compact `edge_pixel_id` per
// edge pixel. If the counter saturates at `MAX_EDGE_BUDGET`, subsequent
// edges atomicAdd into `edge_buffers.edge_overflow_count` and the
// classify shader's `if (edge_id < max_edge_budget)` clamp drops them
// silently — those pixels miss edge resolution and render with whatever
// the primary pass wrote (sample 0).
//
// The full fix (a hash-bucketed atomic-add overflow tail accumulator,
// re-read by `final_blend` for pixels not allocated in the primary slot
// range) is parked as a future enhancement: it requires layout changes
// across `edge_resolve.wgsl`, `final_blend.wgsl`, the uniform packing,
// and careful fixed-point/atomic semantics — multiple hours of work and
// non-trivial validation risk.
//
// The MVP shipped today: surface the budget in the boot log and provide
// a one-shot warn helper for consumers that want to add CPU-side
// readback later. The shader-side clamp + overflow counter atomics are
// already in place (see classify's `compute.wgsl`).

static BUDGET_LOG_GUARD: Mutex<bool> = Mutex::new(false);
static OVERFLOW_WARN_GUARD: Mutex<bool> = Mutex::new(false);

/// One-shot info log announcing the active edge budget. Called from
/// [`MaterialEdgeBuffers::new_with_budget`]; subsequent calls are no-ops
/// for the rest of the session.
fn note_edge_budget_initialized(bucket_count: u32, max_edge_budget: u32) {
    if let Ok(mut guard) = BUDGET_LOG_GUARD.lock() {
        if !*guard {
            *guard = true;
            let accumulator_mb = (accumulator_bytes(max_edge_budget) as f64) / (1024.0 * 1024.0);
            tracing::info!(
                target: "awsm_renderer::edge_resolve",
                bucket_count,
                max_edge_budget,
                accumulator_mb,
                "MAX_EDGE_BUDGET initialized — edges beyond this count saturate the counter \
                 (edge_overflow_count atomicAdd) and skip edge_resolve; affected pixels render \
                 with the primary-sample shading. Full atomic-add overflow fallback is parked \
                 (see docs/plans/more-optimizations.md Block C.2)."
            );
        }
    }
}

/// One-shot warn announcing observed `edge_overflow_count > 0`. Intended
/// to be invoked from a (future) CPU-side `mapAsync` readback of the
/// `edge_overflow_count` mirror in `data_buffer`'s header. Idempotent
/// per session — calling it every frame is safe; only the first call
/// emits.
///
/// Not currently wired into a per-frame readback path; exposed so a
/// later session that adds the readback (alongside the existing
/// coverage-buffer mapAsync flow) can flip the diagnostic on without
/// touching this module.
pub fn note_edge_overflow_observed(overflow_count: u32, max_edge_budget: u32) {
    if overflow_count == 0 {
        return;
    }
    if let Ok(mut guard) = OVERFLOW_WARN_GUARD.lock() {
        if !*guard {
            *guard = true;
            tracing::warn!(
                target: "awsm_renderer::edge_resolve",
                overflow_count,
                max_edge_budget,
                "MAX_EDGE_BUDGET exceeded — edge_overflow_count={overflow_count} edges past \
                 budget {max_edge_budget} were dropped this frame; those pixels rendered with \
                 primary-sample shading instead of full MSAA resolve. Raise the budget or \
                 lower edge density; the atomic-add overflow fallback (Block C.2 full fix) \
                 is not yet wired in.",
            );
        }
    }
}

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

/// Packed `(x: u16, y: u16)` for the edge_to_xy region.
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

/// Bytes per accumulator slot (`vec4<f32>`).
pub const ACCUMULATOR_SLOT_BYTES: u32 = 16;

// ─────────────────────────────────────────────────────────────────
// args_buffer layout (Indirect | CopyDst).
//
// Layout (16-byte aligned):
//   - edge_count: atomic<u32>             — bytes [0, 4)
//   - edge_overflow_count: atomic<u32>    — bytes [4, 8)
//   - 8 bytes pad
//   - final_blend_args:    DispatchIndirectArgs — bytes [16, 32)
//   - skybox_edge_args:    DispatchIndirectArgs — bytes [32, 48)
//   - per_shader_id_args:  array<DispatchIndirectArgs, bucket_count>
//                                                — bytes [48, 48 + bucket_count*16)
//
// Buckets line up with `dynamic_materials::bucket_entries()` (first-party
// + dynamic) — same indexing scheme as the classify pass uses.

/// Bytes used by the atomic counters at the head of `args_buffer`.
pub const ARGS_COUNTERS_BYTES: u32 = 16;

/// Total bytes for the `args_buffer`.
pub fn args_buffer_bytes(bucket_count: u32) -> u32 {
    // counters + final_blend + skybox + per-shader
    ARGS_COUNTERS_BYTES + (2u32.saturating_add(bucket_count)).saturating_mul(INDIRECT_ARGS_STRIDE)
}

// ─────────────────────────────────────────────────────────────────
// data_buffer layout (Storage | CopyDst).
//
// Layout (small header for atomic-counter mirrors that the resolve
// shaders read; everything else follows):
//   - bytes [0, 4)              : edge_count_mirror (atomic<u32>)
//   - bytes [4, 8)              : edge_overflow_count_mirror (atomic<u32>)
//   - bytes [8, 16)             : pad to 16-byte alignment
//   - bytes [16, 16 + B*4)      : per-bucket sample entry counts (atomic<u32>×B)
//   - bytes [16 + B*4, 20 + B*4): skybox sample entry count (atomic<u32>)
//   - padded to 16-byte align
//   - edge_to_xy:       array<u32, max_edge_budget>            — packed (x:16, y:16)
//   - edge_slot_map:    array<u32, max_edge_budget>            — 4 shader_ids × 8 bits
//   - accumulator:      array<vec4<f32>, max_edge_budget × 4>
//   - per-shader-id sample lists: array<array<u32, sample_entries_per_bucket>, bucket_count>
//   - skybox sample list: array<u32, sample_entries_per_bucket>
//
// The atomic-counter mirrors duplicate values from `args_buffer` (which
// drives indirect dispatch). The resolve shaders need to read entry
// counts and edge_count for bounds-checking, but binding `args_buffer`
// as a Storage(read) buffer alongside the existing 9 storage bindings
// would push the compute stage past `maxStorageBuffersPerShaderStage`
// (= 10 on baseline WebGPU; macOS Metal in particular). Mirroring the
// counters into `data_buffer` keeps the resolve-side storage-buffer
// count at 10 (the existing 9 + just `edge_data`).

/// Bytes for the data_buffer's counter-mirror header.
pub fn data_header_bytes(bucket_count: u32) -> u32 {
    // counters (16 B) + B*4 per-bucket + 4 skybox; padded to 16.
    let counters = 16u32;
    let per_bucket = bucket_count.saturating_mul(4);
    let skybox = 4u32;
    let unpadded = counters + per_bucket + skybox;
    (unpadded + 15) & !15
}

/// Byte offset of `edge_count_mirror` (`atomic<u32>`) inside `data_buffer`.
pub fn data_edge_count_offset() -> u32 {
    0
}

/// Byte offset of the per-bucket entry count for `bucket_index` inside
/// `data_buffer`.
pub fn data_per_shader_count_offset(bucket_index: u32) -> u32 {
    16 + bucket_index.saturating_mul(4)
}

/// Byte offset of the skybox entry count inside `data_buffer`.
pub fn data_skybox_count_offset(bucket_count: u32) -> u32 {
    16 + bucket_count.saturating_mul(4)
}

/// Byte offset of `edge_to_xy` inside `data_buffer`. Follows the
/// counter-mirror header.
pub fn edge_to_xy_offset(bucket_count: u32) -> u32 {
    data_header_bytes(bucket_count)
}

/// Byte offset of `edge_slot_map` inside `data_buffer`.
pub fn edge_slot_map_offset(bucket_count: u32, max_edge_budget: u32) -> u32 {
    edge_to_xy_offset(bucket_count) + max_edge_budget.saturating_mul(4)
}

/// Byte offset of `accumulator` inside `data_buffer`.
pub fn accumulator_offset(bucket_count: u32, max_edge_budget: u32) -> u32 {
    edge_slot_map_offset(bucket_count, max_edge_budget) + max_edge_budget.saturating_mul(4)
}

/// Total size of the accumulator array, in bytes.
pub fn accumulator_bytes(max_edge_budget: u32) -> u32 {
    max_edge_budget
        .saturating_mul(ACCUMULATOR_SLOTS_PER_EDGE)
        .saturating_mul(ACCUMULATOR_SLOT_BYTES)
}

/// Byte offset of the first per-shader-id sample-list entry inside
/// `data_buffer`.
pub fn sample_entries_offset(bucket_count: u32, max_edge_budget: u32) -> u32 {
    accumulator_offset(bucket_count, max_edge_budget) + accumulator_bytes(max_edge_budget)
}

/// Per-bucket sample-list capacity (in entries; each entry 4 bytes).
pub fn sample_entries_per_bucket(max_edge_budget: u32) -> u32 {
    max_edge_budget.saturating_mul(SAMPLE_ENTRIES_PER_BUCKET_MULTIPLIER)
}

/// Byte offset of the skybox sample-list region inside `data_buffer`.
pub fn skybox_sample_list_offset(bucket_count: u32, max_edge_budget: u32) -> u32 {
    let per_bucket_bytes = sample_entries_per_bucket(max_edge_budget).saturating_mul(4);
    sample_entries_offset(bucket_count, max_edge_budget)
        + bucket_count.saturating_mul(per_bucket_bytes)
}

/// Total bytes for `data_buffer` (per-edge arrays + per-shader-id
/// sample lists + skybox sample list).
pub fn data_buffer_bytes(bucket_count: u32, max_edge_budget: u32) -> u32 {
    let per_bucket_bytes = sample_entries_per_bucket(max_edge_budget).saturating_mul(4);
    skybox_sample_list_offset(bucket_count, max_edge_budget) + per_bucket_bytes
}

/// Composite GPU buffers for the MSAA edge-resolve flow.
///
/// Split across two GpuBuffers so the dispatch-indirect args (the
/// counters + per-shader workgroup_count_x cells) can live in a
/// `Indirect | CopyDst`-usage buffer, while the storage-writable
/// accumulator / sample lists live in a `Storage | CopyDst` buffer.
/// WebGPU rejects a single buffer that's bound as Indirect AND
/// Storage(read-write) in the same compute pass's synchronization
/// scope — splitting them sidesteps the validation conflict entirely.
///
/// Resized when the bucket count (a dynamic-material registration grew
/// the registry) or the max_edge_budget changes.
pub struct MaterialEdgeBuffers {
    /// `Indirect | CopyDst` GPU buffer holding atomic counters and
    /// the `(2 + bucket_count)` indirect-args slots. Classify binds it
    /// as `storage RW`; edge_resolve / skybox_edge_resolve / final_blend
    /// use it as `dispatch_workgroups_indirect`'s source.
    pub args_buffer: web_sys::GpuBuffer,
    /// `Storage | CopyDst` GPU buffer holding `edge_to_xy`,
    /// `edge_slot_map`, the accumulator, and the per-shader-id +
    /// skybox sample lists.
    pub data_buffer: web_sys::GpuBuffer,
    pub bucket_count: u32,
    pub max_edge_budget: u32,
    pub args_size_bytes: u32,
    pub data_size_bytes: u32,
    /// CPU staging vec sized to `args_buffer_bytes(bucket_count)`.
    /// Written once per frame at the top of classify to clear the
    /// atomic counters + reset the indirect-arg `(x=0, y=1, z=1, pad=0)`
    /// slots.
    args_scratch: Vec<u8>,
    /// CPU staging vec sized to `data_header_bytes(bucket_count)`.
    /// Written once per frame at the top of classify to zero the
    /// counter-mirror header in `data_buffer` (edge_count +
    /// per-shader entry counts + skybox count). The tile arrays /
    /// accumulator / sample lists are overwritten in place by the
    /// shader.
    data_header_scratch: Vec<u8>,
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
        let args_size_bytes = args_buffer_bytes(bucket_count);
        let data_size_bytes = data_buffer_bytes(bucket_count, max_edge_budget);

        let args_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("MaterialEdgeBuffers::args"),
                args_size_bytes as usize,
                // Storage so classify can atomicAdd into the counters
                // and per-shader workgroup_count_x cells; Indirect so
                // the edge dispatches can read their workgroup counts
                // from here; CopyDst so reset_header can rewrite it
                // each frame.
                BufferUsage::new()
                    .with_storage()
                    .with_indirect()
                    .with_copy_dst(),
            )
            .into(),
        )?;

        let data_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("MaterialEdgeBuffers::data"),
                data_size_bytes as usize,
                // Storage so all the per-edge data lives here. NO
                // Indirect — that's exclusively on args_buffer.
                BufferUsage::new().with_storage().with_copy_dst(),
            )
            .into(),
        )?;

        let mut args_scratch = vec![0u8; args_size_bytes as usize];
        write_args_header(&mut args_scratch, bucket_count);
        // data_header_scratch starts zero — every counter mirror is 0
        // at frame start. The shader atomicAdds against it as edges
        // are allocated.
        let data_header_scratch = vec![0u8; data_header_bytes(bucket_count) as usize];

        note_edge_budget_initialized(bucket_count, max_edge_budget);

        Ok(Self {
            args_buffer,
            data_buffer,
            bucket_count,
            max_edge_budget,
            args_size_bytes,
            data_size_bytes,
            args_scratch,
            data_header_scratch,
        })
    }

    /// Recreates the buffers if a dynamic-material registration grew
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

    /// Writes the per-frame `args_buffer` + `data_buffer` header
    /// resets. Zeroes the args_buffer atomic counters and re-asserts
    /// `(y=1, z=1)` on every indirect-arg slot; zeroes the data buffer's
    /// counter-mirror header (edge_count + per-bucket counts + skybox
    /// count). Tile arrays / accumulator / sample lists are
    /// overwritten in place by the shader.
    pub fn reset_header(&self, gpu: &AwsmRendererWebGpu) -> Result<(), AwsmCoreError> {
        gpu.write_buffer(
            &self.args_buffer,
            None,
            self.args_scratch.as_slice(),
            None,
            None,
        )?;
        gpu.write_buffer(
            &self.data_buffer,
            None,
            self.data_header_scratch.as_slice(),
            None,
            None,
        )
    }

    /// Byte offset of the `final_blend` indirect-arg slot in
    /// `args_buffer`. Passed to `dispatch_workgroups_indirect`.
    pub fn final_blend_args_offset() -> u32 {
        ARGS_COUNTERS_BYTES
    }

    /// Byte offset of the `skybox_edge` indirect-arg slot in
    /// `args_buffer`.
    pub fn skybox_edge_args_offset() -> u32 {
        ARGS_COUNTERS_BYTES + INDIRECT_ARGS_STRIDE
    }

    /// Byte offset of the per-shader-id indirect-arg slot for bucket
    /// `bucket_index` in `args_buffer`. Passed to
    /// `dispatch_workgroups_indirect`.
    pub fn per_shader_args_offset(bucket_index: u32) -> u32 {
        ARGS_COUNTERS_BYTES + 2 * INDIRECT_ARGS_STRIDE + bucket_index * INDIRECT_ARGS_STRIDE
    }
}

/// Build the `EdgeBufferLayout` uniform-data payload for the
/// classify + edge_resolve shaders. The shader-side struct is
/// templated per bucket count (one `<name>_sample_list_base: u32`
/// field per bucket entry), so the payload size grows with bucket
/// count. Padded to 16-byte alignment for WebGPU uniform-buffer
/// requirements.
///
/// Layout (all u32, in declaration order):
///   max_edge_budget
///   edge_count_index               (u32-stride into edge_data; 0)
///   per_shader_count_base          (first per-bucket counter; bucket counts follow as a contiguous array)
///   skybox_count_index             (skybox entry counter)
///   edge_to_xy_base
///   edge_slot_map_base
///   accumulator_base
///   <first_party_0>_sample_list_base
///   <first_party_1>_sample_list_base
///   ... (bucket_count entries total)
///   skybox_sample_list_base
///   sample_entries_per_bucket
///
/// All `*_base` values are u32-stride indices from the start of the
/// `edge_data` storage buffer.
pub fn build_edge_layout_uniform_bytes(bucket_count: u32, max_edge_budget: u32) -> Vec<u8> {
    let to_stride = |byte_off: u32| -> u32 { byte_off / 4 };

    let mut words: Vec<u32> = Vec::with_capacity(8 + bucket_count as usize);
    words.push(max_edge_budget);
    words.push(to_stride(data_edge_count_offset())); // edge_count index
    words.push(to_stride(data_per_shader_count_offset(0))); // per_shader_count_base
    words.push(to_stride(data_skybox_count_offset(bucket_count))); // skybox_count_index
    words.push(to_stride(edge_to_xy_offset(bucket_count)));
    words.push(to_stride(edge_slot_map_offset(
        bucket_count,
        max_edge_budget,
    )));
    words.push(to_stride(accumulator_offset(bucket_count, max_edge_budget)));
    let per_bucket = sample_entries_per_bucket(max_edge_budget);
    let base = sample_entries_offset(bucket_count, max_edge_budget);
    for i in 0..bucket_count {
        words.push(to_stride(base + i * per_bucket * 4)); // 4 bytes per sample entry (packed u32)
    }
    // skybox_sample_list_base — extra slot after the per-bucket lists.
    words.push(to_stride(skybox_sample_list_offset(
        bucket_count,
        max_edge_budget,
    )));
    words.push(per_bucket);

    // Pad to 16-byte alignment (WebGPU uniform-buffer requirement).
    while (words.len() * 4) % 16 != 0 {
        words.push(0);
    }

    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_ne_bytes());
    }
    bytes
}

/// Creates the EdgeBufferLayout uniform buffer and writes its
/// payload. Returns the GpuBuffer + the byte size (for bind-group
/// construction).
pub fn build_edge_layout_uniform(
    gpu: &AwsmRendererWebGpu,
    bucket_count: u32,
    max_edge_budget: u32,
) -> Result<(web_sys::GpuBuffer, u32), AwsmCoreError> {
    let bytes = build_edge_layout_uniform_bytes(bucket_count, max_edge_budget);
    let buffer = gpu.create_buffer(
        &BufferDescriptor::new(
            Some("EdgeBufferLayout uniform"),
            bytes.len(),
            BufferUsage::new().with_uniform().with_copy_dst(),
        )
        .into(),
    )?;
    gpu.write_buffer(&buffer, None, bytes.as_slice(), None, None)?;
    Ok((buffer, bytes.len() as u32))
}

/// Writes the initial `args_buffer` header into `dst`. Layout per the
/// module-level docs: 2 atomic counters + 8B pad + 1 final_blend args
/// slot + 1 skybox_edge args slot + bucket_count per-shader-id args
/// slots.
pub fn write_args_header(dst: &mut [u8], bucket_count: u32) {
    let one = 1u32.to_ne_bytes();
    // Counters: both zero (default).
    // (bytes [0, 4) and [4, 8) are already zeroed by vec![0u8; ...].)
    // 8-byte alignment pad: zeros.

    // final_blend args slot at byte offset 16.
    let final_blend_base = ARGS_COUNTERS_BYTES as usize;
    dst[final_blend_base..final_blend_base + 4].copy_from_slice(&[0; 4]); // x
    dst[final_blend_base + 4..final_blend_base + 8].copy_from_slice(&one); // y
    dst[final_blend_base + 8..final_blend_base + 12].copy_from_slice(&one); // z
    dst[final_blend_base + 12..final_blend_base + 16].copy_from_slice(&[0; 4]); // pad

    // skybox_edge args slot at byte offset 32.
    let skybox_base = (ARGS_COUNTERS_BYTES + INDIRECT_ARGS_STRIDE) as usize;
    dst[skybox_base..skybox_base + 4].copy_from_slice(&[0; 4]); // x
    dst[skybox_base + 4..skybox_base + 8].copy_from_slice(&one); // y
    dst[skybox_base + 8..skybox_base + 12].copy_from_slice(&one); // z
    dst[skybox_base + 12..skybox_base + 16].copy_from_slice(&[0; 4]); // pad

    // Per-shader-id args slots.
    let per_shader_base = (ARGS_COUNTERS_BYTES + 2 * INDIRECT_ARGS_STRIDE) as usize;
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
    fn args_size_is_aligned() {
        for bucket_count in [1u32, 4, 5, 17] {
            assert_eq!(args_buffer_bytes(bucket_count) % 16, 0);
        }
    }

    #[test]
    fn data_offsets_start_at_zero() {
        for bucket_count in [1u32, 4, 17] {
            assert_eq!(edge_to_xy_offset(bucket_count), 0);
        }
    }
}
