//! Renderer-wide variable-length per-material data buffer.
//!
//! Custom materials with `BufferSlot` declarations get a contiguous
//! `u32` slice in this pool; the auto-generated `MaterialData` struct
//! exposes them as `<slot>_offset: u32` + `<slot>_length: u32`. WGSL
//! reads via `extras_load_f32(material.<slot>_offset + i)` /
//! `extras_load_u32(...)`.
//!
//! ## Status
//!
//! Ships a bump allocator + per-slice `(offset, length)` tracking.
//! Compaction + free-list re-use of removed slices is parked as a
//! follow-up — the bump allocator grows the pool when it overflows.
//! Most scenes won't trigger growth at all (the 1 MiB default holds
//! ~262k u32s, easily enough for hand-authored sprite atlases).
//!
//! The pool integrates with [`MappedUploader`]: bulk inserts use
//! `ingest_foreign` (foreign-bytes ingestion), per-frame edits use
//! `write_dirty_ranges`.

use std::collections::HashMap;
use std::sync::LazyLock;

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};
use awsm_renderer_materials::MaterialShaderId;

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
    /// Exact-size free list — slices reclaimed by [`Self::drop_shader`]
    /// when a `MaterialShaderId` is unregistered (the editor's
    /// edit→re-register cycle is the most common producer). Keyed by
    /// slice length in words; values are LIFO stacks of free offsets.
    ///
    /// Same-length re-allocation is the dominant case (a material's
    /// `BufferSlot` layout doesn't change between edits — only the
    /// bytes do). Free entries at one length don't satisfy a request
    /// at a different length; those gaps fragment until the bump
    /// allocator overflows and grows past them. Full best-fit /
    /// coalescing compaction is parked as a future enhancement; the
    /// exact-size path handles the common case at trivial cost.
    free_list: HashMap<u32, Vec<u32>>,
    /// Bytes of `shadow` that are dirty (range in bytes — same units
    /// `DynamicStorageBuffer::take_dirty_ranges` returns).
    dirty_range: Option<(u32, u32)>,
    uploader: MappedUploader,
}

impl ExtrasPool {
    /// Create an empty extras pool sized to `capacity_words`. The GPU
    /// buffer is allocated up front; bump allocations consume
    /// contiguous regions of it.
    pub fn new(gpu: &AwsmRendererWebGpu, capacity_words: u32) -> Result<Self, AwsmCoreError> {
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
            free_list: HashMap::new(),
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
    pub fn slice_for(&self, shader_id: MaterialShaderId, slot_index: usize) -> Option<(u32, u32)> {
        self.slices.get(&(shader_id, slot_index)).copied()
    }

    /// Assign a slice to `(shader_id, slot_index)` and copy `data`
    /// into the shadow + dirty range. If a slice was previously
    /// assigned and its length matches, the existing offset is
    /// reused. Otherwise a fresh slice is bump-allocated; on
    /// allocator overflow the pool doubles its GPU capacity and
    /// retries (so the per-call contract is "this always succeeds
    /// for any reasonable `data.len()`").
    pub fn assign_or_update(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        shader_id: MaterialShaderId,
        slot_index: usize,
        data: &[u32],
    ) -> Result<AssignOutcome, ExtrasPoolError> {
        let len = data.len() as u32;
        let key = (shader_id, slot_index);
        let mut resized = false;
        let (offset, length) = match self.slices.get(&key) {
            Some(&(off, prev_len)) if prev_len == len => (off, prev_len),
            // Either the key isn't assigned yet, or its previous slice
            // is the wrong length and needs to be replaced. In the
            // latter case the old slice goes onto the free list so a
            // future same-length allocation can reclaim it.
            existing => {
                if let Some(&(old_off, old_len)) = existing {
                    self.free_list.entry(old_len).or_default().push(old_off);
                    self.slices.remove(&key);
                }
                // Prefer a free-list hit at the exact length — pops the
                // most recently freed offset (LIFO) so locally adjacent
                // edit→re-register cycles tend to land on the same
                // bytes.
                if let Some(stack) = self.free_list.get_mut(&len) {
                    if let Some(offset) = stack.pop() {
                        self.slices.insert(key, (offset, len));
                        (offset, len)
                    } else {
                        // Stack empty — fall through to bump.
                        self.bump_allocate(gpu, key, len, &mut resized)?
                    }
                } else {
                    self.bump_allocate(gpu, key, len, &mut resized)?
                }
            }
        };
        let start = offset as usize;
        let end = start + length as usize;
        self.shadow[start..end].copy_from_slice(data);

        // Mark the byte range as dirty for the next per-frame upload.
        // On resize the whole live range is already covered (see
        // `grow_to`), so the further extension here is a no-op.
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
        Ok(AssignOutcome {
            offset,
            length,
            resized,
        })
    }

    /// Bump-allocate a fresh slice of `len` words; grows the pool if
    /// the bump overflows. Sets `*resized` to true when the grow path
    /// is taken so the caller can mark the corresponding bind group
    /// recreation event.
    fn bump_allocate(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        key: (MaterialShaderId, usize),
        len: u32,
        resized: &mut bool,
    ) -> Result<(u32, u32), ExtrasPoolError> {
        let offset = self.next_offset;
        let new_next = offset.saturating_add(len);
        if new_next > self.capacity_words {
            // Grow until the next free offset fits. Two halts: we double
            // on each iteration (so `new_next` is reached in O(log)
            // steps); and `len` can never exceed the addressable u32
            // range without first overflowing `new_next`, which we'd
            // have caught via the `saturating_add` above.
            let mut new_capacity = self.capacity_words;
            while new_capacity < new_next {
                new_capacity =
                    new_capacity
                        .checked_mul(2)
                        .ok_or(ExtrasPoolError::OutOfCapacity {
                            needed: new_next,
                            capacity: self.capacity_words,
                        })?;
            }
            self.grow_to(gpu, new_capacity)?;
            *resized = true;
        }
        self.next_offset = offset.saturating_add(len);
        self.slices.insert(key, (offset, len));
        Ok((offset, len))
    }

    /// Reclaim every slice owned by `shader_id`. Called from
    /// `DynamicMaterials::unregister_material` to release per-slot
    /// allocations back to the free list when a `MaterialShaderId`
    /// is unregistered (editor edit→re-register cycle, scene teardown,
    /// etc.). Slices land in the same exact-size free list used by
    /// [`Self::assign_or_update`]; the next equal-length allocation
    /// reclaims them.
    ///
    /// Returns the number of slices reclaimed (zero when the shader
    /// had no `BufferSlot`s, or had already been dropped).
    pub fn drop_shader(&mut self, shader_id: MaterialShaderId) -> usize {
        let drained: Vec<((MaterialShaderId, usize), (u32, u32))> = self
            .slices
            .iter()
            .filter(|((sid, _), _)| *sid == shader_id)
            .map(|(k, v)| (*k, *v))
            .collect();
        let count = drained.len();
        for (key, (offset, len)) in drained {
            self.slices.remove(&key);
            self.free_list.entry(len).or_default().push(offset);
        }
        count
    }

    /// Grow the pool to at least `new_capacity_words`. Reallocates the
    /// GPU buffer, widens the shadow vec, and marks the live prefix
    /// (`0..next_offset`) dirty so the next-frame upload re-populates
    /// the fresh buffer. The shadow is the authoritative byte source —
    /// no `copyBufferToBuffer` is needed.
    fn grow_to(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        new_capacity_words: u32,
    ) -> Result<(), ExtrasPoolError> {
        debug_assert!(new_capacity_words > self.capacity_words);
        let new_buffer = gpu
            .create_buffer(
                &BufferDescriptor::new(
                    Some("ExtrasPool"),
                    (new_capacity_words as usize) * 4,
                    *BUFFER_USAGE,
                )
                .into(),
            )
            .map_err(ExtrasPoolError::Core)?;
        self.buffer = new_buffer;
        self.capacity_words = new_capacity_words;
        self.shadow.resize(new_capacity_words as usize, 0);
        // Re-dirty everything currently live so the next upload
        // re-populates the new buffer. The MappedUploader's per-frame
        // path discards any in-flight staging that targeted the old
        // buffer (the old buffer is dropped here; web-sys's GpuBuffer
        // is a refcount handle, so any prior submitted command-encoder
        // recording that referenced the old handle still works for
        // that frame — but the next frame's writes go to the new
        // handle via `self.buffer` and the bind groups recreated on
        // `ExtrasPoolResize`).
        if self.next_offset > 0 {
            let live_end_bytes = self.next_offset * 4;
            self.dirty_range = match self.dirty_range {
                Some((_, e)) => Some((0, e.max(live_end_bytes))),
                None => Some((0, live_end_bytes)),
            };
        }
        tracing::info!(
            target: "awsm_renderer::extras_pool",
            "ExtrasPool grew to {} words ({} bytes); live bytes={}",
            new_capacity_words,
            (new_capacity_words as usize) * 4,
            self.next_offset * 4,
        );
        Ok(())
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

/// Outcome of [`ExtrasPool::assign_or_update`]: the slice assignment
/// plus a `resized` flag the caller uses to mark
/// [`crate::bind_groups::BindGroupCreate::ExtrasPoolResize`] when the
/// underlying GPU buffer was grown.
///
/// The shadow `Vec<u32>` is the authoritative source of every live
/// slice's bytes, so a resize doesn't need `copyBufferToBuffer` from
/// the old buffer — we widen the shadow, mark the live range dirty,
/// and let the next-frame upload re-populate the fresh buffer.
#[derive(Clone, Copy, Debug)]
pub struct AssignOutcome {
    /// Offset into the pool (in u32 words) at which the assigned
    /// slice begins.
    pub offset: u32,
    /// Length of the slice in u32 words.
    pub length: u32,
    /// `true` if the pool grew on this call (the GPU buffer was
    /// reallocated). Callers MUST mark
    /// [`crate::bind_groups::BindGroupCreate::ExtrasPoolResize`]
    /// when this is `true` — otherwise the opaque + transparent
    /// main bind groups silently keep pointing at the dropped
    /// buffer handle.
    pub resized: bool,
}

/// Errors produced by the extras-pool allocator.
#[derive(Debug, thiserror::Error)]
pub enum ExtrasPoolError {
    /// The bump allocator's grow path overflowed `u32` capacity — the
    /// pool can never reach the requested size. In practice this is
    /// never hit: 4 GiB of u32 words is more than any author would ask
    /// for, and the grow path stops well before that.
    #[error("[extras-pool] out of capacity: needed {needed} words, capacity {capacity}")]
    OutOfCapacity {
        /// Total words the allocator needed for the next slice.
        needed: u32,
        /// Current pool capacity in words.
        capacity: u32,
    },
    /// GPU buffer (re)creation failed during a grow. The pool stays at
    /// its previous capacity; the caller's `assign_or_update` returns
    /// without inserting the slice.
    #[error("[extras-pool] gpu buffer creation failed: {0:?}")]
    Core(AwsmCoreError),
}
