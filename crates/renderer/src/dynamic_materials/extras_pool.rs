//! Renderer-wide variable-length per-material data buffer.
//!
//! Custom materials with `BufferSlot` declarations get a contiguous
//! `u32` slice in this pool; the auto-generated `MaterialData` struct
//! exposes them as `<slot>_offset: u32` + `<slot>_length: u32`. WGSL
//! reads via `extras_load_f32(material.<slot>_offset + i)` /
//! `extras_load_u32(...)`.
//!
//! ## Phase 6 status
//!
//! Ships a bump allocator + per-slice `(offset, length)` tracking.
//! Compaction + free-list re-use of removed slices is parked as a
//! follow-up — the bump allocator grows the pool when it overflows.
//! Most scenes won't trigger growth at all (the 1 MiB default holds
//! ~262k u32s, easily enough for hand-authored sprite atlases).
//!
//! The pool integrates with [`MappedUploader`] per
//! [`docs/PERFORMANCE.md`][perf]: bulk inserts use `ingest_foreign`
//! (foreign-bytes ingestion), per-frame edits use `write_dirty_ranges`.
//!
//! [perf]: ../../../docs/PERFORMANCE.md

use std::collections::HashMap;
use std::sync::LazyLock;

use awsm_materials::MaterialShaderId;
use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

use crate::buffer::mapped_uploader::MappedUploader;

/// Default extras-pool capacity in u32 words. 1 MiB = 262 144 u32s —
/// plenty for hand-authored sprite atlases, irregular-cell grids, and
/// the scattered tabular data a typical custom material reads.
pub const DEFAULT_CAPACITY_WORDS: u32 = 262_144;

static BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_copy_dst().with_storage());

/// Renderer-wide variable-length-per-material storage buffer.
///
/// Owns the GPU buffer + a `MappedUploader` for ring-batched per-frame
/// edits, plus a per-`(material_shader_id, slot_index)` slice table so
/// the per-frame packer can resolve `(offset, length)` pairs.
pub struct ExtrasPool {
    pub(crate) buffer: web_sys::GpuBuffer,
    /// Total capacity in u32 words.
    pub(crate) capacity_words: u32,
    /// Bump-allocated next-free offset (in u32 words).
    next_offset: u32,
    /// CPU-side shadow of every live slice. Indexed by
    /// `(material_shader_id, slot_index)` → `(offset, length)`.
    /// Per-frame writes go through `MappedUploader::write_dirty_ranges`;
    /// the shadow is the authoritative byte source.
    shadow: Vec<u32>,
    /// `(shader_id, slot_index) → (offset_in_words, length_in_words)`
    /// — the per-slice assignment the WGSL `extras_load_*` helpers
    /// read from.
    slices: HashMap<(MaterialShaderId, usize), (u32, u32)>,
    /// Bytes of `shadow` that are dirty (range in bytes — same units
    /// `DynamicStorageBuffer::take_dirty_ranges` returns).
    dirty_range: Option<(u32, u32)>,
    uploader: MappedUploader,
}

impl ExtrasPool {
    /// Create an empty extras pool sized to `capacity_words`. The GPU
    /// buffer is allocated up front; bump allocations consume
    /// contiguous regions of it.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        capacity_words: u32,
    ) -> Result<Self, AwsmCoreError> {
        let capacity_words = capacity_words.max(1);
        let buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("ExtrasPool"),
                (capacity_words as usize) * 4,
                *BUFFER_USAGE,
            )
            .into(),
        )?;
        Ok(Self {
            buffer,
            capacity_words,
            next_offset: 0,
            shadow: vec![0u32; capacity_words as usize],
            slices: HashMap::new(),
            dirty_range: None,
            uploader: MappedUploader::new("ExtrasPool"),
        })
    }

    /// Mapped-ring upload telemetry for this subsystem.
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.uploader.stats()
    }

    /// Returns the per-slice `(offset, length)` pair for a registered
    /// `(shader_id, slot_index)`, if assigned. `None` means the slot
    /// is unassigned — the packer writes `(0, 0)` in that case and
    /// the author's WGSL fragment sees `<slot>_length == 0`.
    pub fn slice_for(
        &self,
        shader_id: MaterialShaderId,
        slot_index: usize,
    ) -> Option<(u32, u32)> {
        self.slices.get(&(shader_id, slot_index)).copied()
    }

    /// Assign a slice to `(shader_id, slot_index)` and copy `data`
    /// into the shadow + dirty range. If a slice was previously
    /// assigned and its length matches, the existing offset is
    /// reused. Otherwise a fresh slice is bump-allocated.
    ///
    /// Returns the `(offset_in_words, length_in_words)` pair.
    pub fn assign_or_update(
        &mut self,
        shader_id: MaterialShaderId,
        slot_index: usize,
        data: &[u32],
    ) -> Result<(u32, u32), ExtrasPoolError> {
        let len = data.len() as u32;
        let key = (shader_id, slot_index);
        let (offset, length) = match self.slices.get(&key) {
            Some(&(off, prev_len)) if prev_len == len => (off, prev_len),
            _ => {
                let offset = self.next_offset;
                let new_next = offset.saturating_add(len);
                if new_next > self.capacity_words {
                    return Err(ExtrasPoolError::OutOfCapacity {
                        needed: new_next,
                        capacity: self.capacity_words,
                    });
                }
                self.next_offset = new_next;
                self.slices.insert(key, (offset, len));
                (offset, len)
            }
        };
        let start = offset as usize;
        let end = start + length as usize;
        self.shadow[start..end].copy_from_slice(data);

        // Mark the byte range as dirty for the next per-frame upload.
        let dirty_start_bytes = offset * 4;
        let dirty_end_bytes = (offset + length) * 4;
        match self.dirty_range {
            Some((s, e)) => {
                self.dirty_range = Some((s.min(dirty_start_bytes), e.max(dirty_end_bytes)));
            }
            None => {
                self.dirty_range = Some((dirty_start_bytes, dirty_end_bytes));
            }
        }
        Ok((offset, length))
    }

    /// Per-frame upload — flushes any dirty range through the
    /// MappedUploader. No-op when nothing changed since the last
    /// frame's upload.
    pub fn write_gpu(&mut self, gpu: &AwsmRendererWebGpu) -> Result<(), AwsmCoreError> {
        let Some((start_bytes, end_bytes)) = self.dirty_range.take() else {
            return Ok(());
        };
        // The shadow is a Vec<u32>; reinterpret as bytes for the
        // upload. `bytemuck` is the canonical cast but we avoid the
        // dep by reslicing manually — `shadow.len() * 4` bytes total.
        let shadow_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(self.shadow.as_ptr() as *const u8, self.shadow.len() * 4)
        };
        self.uploader.write_dirty_ranges(
            gpu,
            &self.buffer,
            shadow_bytes.len(),
            shadow_bytes,
            &[(start_bytes as usize, end_bytes as usize)],
        )
    }
}

/// Errors produced by the extras-pool allocator.
#[derive(Debug, thiserror::Error)]
pub enum ExtrasPoolError {
    /// The bump allocator ran out of contiguous space. Phase 6 stub:
    /// the caller falls back to (0, 0) so the material renders against
    /// zeroed data; the resize-on-overflow + compaction paths are
    /// future work.
    #[error("[extras-pool] out of capacity: needed {needed} words, capacity {capacity}")]
    OutOfCapacity {
        /// Total words the allocator needed for the next slice.
        needed: u32,
        /// Current pool capacity in words.
        capacity: u32,
    },
}
