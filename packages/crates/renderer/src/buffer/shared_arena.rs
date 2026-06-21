//! Chunked, stable-address arena with a per-slot seqlock and a coarse
//! chunk dirty bitmap — the shared-memory sim-state primitive
//! (`docs/plans/multithreading.md`, Layer 2, decisions B/C/D).
//!
//! ## What problem this solves
//!
//! The single-threaded mirror is one growable `Vec<u8>`; `resize()`
//! reallocs and the base pointer moves — fatal for a *foreign* writer (a
//! physics worker on another thread) holding an offset into shared linear
//! memory. This arena replaces that with:
//!
//! - **Stable addressing (decision B).** Storage is a list of fixed-size
//!   **chunks**, each its own heap allocation. A slot's address never
//!   moves once assigned; growth appends a new chunk (existing chunks stay
//!   put). A `slot → (chunk, offset)` binding is valid forever.
//! - **Topology is owner-only (decision C).** [`SharedArena::allocate`] /
//!   [`SharedArena::free`] (slot/free-list/grow) are `&mut self` — the
//!   owner (render worker) only. Foreign threads call [`foreign_write`] on
//!   an already-bound slot: value bytes + seqlock + dirty bit, nothing
//!   topological. The hot path touches zero topology.
//! - **Seqlock = publication + dirty (decision D).** One `AtomicU32` per
//!   slot. The writer bumps it odd → writes → bumps it even
//!   (release/acquire). A reader treats `version != last-seen` as dirty
//!   and an odd-or-unstable version as torn (reuse last frame's value —
//!   one-frame staleness that self-heals). One atomic solves tearing
//!   **and** change detection.
//! - **Coarse chunk dirty bitmap (decision D).** One bit per chunk
//!   (`AtomicU32` words, set via atomic-or). The reader descends only
//!   dirty chunks, so scan cost tracks *touched* chunks, not total slots.
//!
//! ## Threading model
//!
//! With the threaded build profile every heap allocation already lives in
//! the one shared `WebAssembly.Memory`, so the chunks/versions/bitmap are
//! shared automatically — only the **addresses** must cross to the foreign
//! writer (once, at slot-binding time; see [`SharedArena::slot_binding`]).
//! On the single-threaded build (and in host `cargo test`) the exact same
//! code runs with no contention; the atomics are simply uncontended.
//!
//! The reader-side bookkeeping (`last_seen` versions + the contiguous
//! `mirror` snapshot) is render-worker-**private** — it is not shared and
//! is what downstream GPU upload consumes (decision E: the arena emits the
//! same `(offset, len)` ranges the existing uploader already takes).

use std::sync::atomic::{fence, AtomicU32, Ordering};

/// Outcome of a single seqlock read attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRead {
    /// Version equals the last-seen version — the slot did not change.
    Unchanged,
    /// A stable new value was read; `version` is the new even version to
    /// record as last-seen.
    Updated { version: u32 },
    /// The read raced a writer (odd version, or the version moved between
    /// the two reads). The caller must reuse the previous value and retry
    /// next frame.
    Torn,
}

/// Seqlock primitives over a bare `&AtomicU32` so they wrap a version cell
/// wherever it lives (a `Vec` on the host, shared wasm memory in the
/// browser). Single-writer-per-slot (the slot's owning body), so no CAS is
/// needed on the write side.
pub mod seqlock {
    use super::*;

    /// Writer: enter a write by publishing an **odd** version. Returns the
    /// odd version to hand back to [`end_write`].
    #[inline]
    pub fn begin_write(version: &AtomicU32) -> u32 {
        let cur = version.load(Ordering::Relaxed);
        // Even -> next odd; if somehow already odd (a re-entrant/partial
        // write), keep it odd. Single writer per slot makes the even case
        // the only real one.
        let odd = if cur & 1 == 0 {
            cur.wrapping_add(1)
        } else {
            cur
        };
        version.store(odd, Ordering::Relaxed);
        // Ensure the value-byte writes that follow are ordered *after* the
        // odd publication.
        fence(Ordering::Release);
        odd
    }

    /// Writer: finish a write by publishing the next **even** version
    /// (release), making the new value visible to readers.
    #[inline]
    pub fn end_write(version: &AtomicU32, odd: u32) {
        version.store(odd.wrapping_add(1), Ordering::Release);
    }

    /// Reader: seqlock-read a slot. `copy` snapshots the value bytes; it is
    /// invoked only when the slot looks changed and stable-so-far, and its
    /// result must be discarded by the caller if the return is
    /// [`SlotRead::Torn`].
    #[inline]
    pub fn read(version: &AtomicU32, last_seen: u32, mut copy: impl FnMut()) -> SlotRead {
        let s1 = version.load(Ordering::Acquire);
        if s1 & 1 == 1 {
            return SlotRead::Torn; // write in progress
        }
        if s1 == last_seen {
            return SlotRead::Unchanged;
        }
        copy();
        fence(Ordering::Acquire);
        let s2 = version.load(Ordering::Acquire);
        if s1 != s2 {
            SlotRead::Torn
        } else {
            SlotRead::Updated { version: s2 }
        }
    }
}

/// Coarse chunk dirty bitmap: one bit per chunk, packed into `AtomicU32`
/// words. Pre-sized to a maximum chunk count so its address is **stable
/// forever** (a foreign writer can cache it). Writers set their chunk's bit
/// with an atomic-or; the reader drains set bits (snapshot + clear).
pub struct DirtyBitmap {
    words: Box<[AtomicU32]>,
}

impl DirtyBitmap {
    /// Capacity for `max_chunks` chunks (rounded up to whole words).
    pub fn new(max_chunks: usize) -> Self {
        let n = max_chunks.div_ceil(32).max(1);
        let words = (0..n).map(|_| AtomicU32::new(0)).collect::<Vec<_>>();
        Self {
            words: words.into_boxed_slice(),
        }
    }

    /// Mark `chunk` dirty (atomic-or — safe from any thread).
    #[inline]
    pub fn mark(&self, chunk: usize) {
        let w = chunk / 32;
        let b = chunk % 32;
        self.words[w].fetch_or(1u32 << b, Ordering::Release);
    }

    /// Is `chunk`'s bit currently set?
    #[inline]
    pub fn is_marked(&self, chunk: usize) -> bool {
        let w = chunk / 32;
        let b = chunk % 32;
        self.words[w].load(Ordering::Acquire) & (1u32 << b) != 0
    }

    /// Snapshot + clear every set bit, appending the dirty chunk indices to
    /// `out` (ascending). A write that lands *after* a word's swap simply
    /// re-sets the bit and is caught next drain — overflow-free, no lost
    /// wakeups beyond a one-frame delay.
    pub fn drain_into(&self, out: &mut Vec<usize>) {
        for (wi, word) in self.words.iter().enumerate() {
            let mut bits = word.swap(0, Ordering::Acquire);
            while bits != 0 {
                let b = bits.trailing_zeros() as usize;
                out.push(wi * 32 + b);
                bits &= bits - 1;
            }
        }
    }

    /// Base address of the bitmap words (for handing to a foreign writer).
    pub fn words_addr(&self) -> usize {
        self.words.as_ptr() as usize
    }
}

/// One fixed-size storage chunk: a contiguous value region plus one version
/// cell per slot. Each chunk is its own heap allocation, so its address is
/// stable for the arena's lifetime.
struct Chunk {
    values: Box<[u8]>,
    versions: Box<[AtomicU32]>,
}

/// Raw addresses a foreign writer needs to write one slot: where the value
/// bytes live, where the slot's version cell lives, and which chunk to mark
/// dirty. Plain `usize`/`Copy` so it round-trips through `postMessage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotBinding {
    /// `*mut u8` — start of this slot's value bytes.
    pub value_addr: usize,
    /// `*const AtomicU32` — this slot's seqlock version cell.
    pub version_addr: usize,
    /// Chunk index (to set the dirty bit).
    pub chunk: usize,
}

/// Foreign (cross-thread) write to a bound slot: bump the seqlock, copy the
/// value bytes, set the chunk dirty bit. This is the entire hot-path write
/// — no topology, no `postMessage`.
///
/// # Safety
/// `binding`'s addresses and `dirty_words_addr` must point into live shared
/// memory (obtained from [`SharedArena::slot_binding`] /
/// [`DirtyBitmap::words_addr`] on the owning arena, which must outlive the
/// write), and `bytes.len()` must equal the arena's stride. The owning
/// thread must not be concurrently reallocating topology for this slot
/// (guaranteed by decision C: topology is owner-only and quiescent during
/// the write loop).
#[inline]
pub unsafe fn foreign_write(binding: SlotBinding, dirty_words_addr: usize, bytes: &[u8]) {
    let version = &*(binding.version_addr as *const AtomicU32);
    let odd = seqlock::begin_write(version);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), binding.value_addr as *mut u8, bytes.len());
    seqlock::end_write(version, odd);
    let word = binding.chunk / 32;
    let bit = binding.chunk % 32;
    let words = dirty_words_addr as *const AtomicU32;
    (*words.add(word)).fetch_or(1u32 << bit, Ordering::Release);
}

/// Result of a [`SharedArena::descend`] pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DescendResult {
    /// Coalesced `(byte_offset, len)` ranges into the contiguous mirror
    /// ([`SharedArena::mirror`]) — exactly the shape the existing GPU
    /// uploader's `write_dirty_ranges` consumes (decision E).
    pub ranges: Vec<(usize, usize)>,
    /// Number of slots that took a fresh value this pass.
    pub updated: usize,
    /// Number of slots that read torn (reused last value, will retry).
    pub torn: usize,
}

/// A chunked, stable-address arena over a per-slot seqlock + chunk dirty
/// bitmap. See the module docs.
pub struct SharedArena {
    stride: usize,
    chunk_slots: usize,
    max_chunks: usize,
    chunks: Vec<Chunk>,
    dirty: DirtyBitmap,
    capacity_slots: usize,
    next_slot: usize,
    free_list: Vec<usize>,

    // Reader-private (NOT shared): the last version accepted per slot and a
    // contiguous `slot * stride` snapshot of accepted values — this mirror
    // is what the GPU uploader reads.
    last_seen: Vec<u32>,
    mirror: Vec<u8>,

    // Scratch reused across descends to avoid per-frame allocation.
    scratch_chunks: Vec<usize>,
    scratch_slot: Vec<u8>,
    scratch_updated: Vec<usize>,
}

impl SharedArena {
    /// Create an arena with `stride` value bytes per slot, `chunk_slots`
    /// slots per chunk, and capacity for up to `max_chunks` chunks (which
    /// fixes the dirty-bitmap address). Starts empty (zero chunks).
    pub fn new(stride: usize, chunk_slots: usize, max_chunks: usize) -> Self {
        assert!(stride > 0 && chunk_slots > 0 && max_chunks > 0);
        Self {
            stride,
            chunk_slots,
            max_chunks,
            chunks: Vec::new(),
            dirty: DirtyBitmap::new(max_chunks),
            capacity_slots: 0,
            next_slot: 0,
            free_list: Vec::new(),
            last_seen: Vec::new(),
            mirror: Vec::new(),
            scratch_chunks: Vec::new(),
            scratch_slot: vec![0u8; stride],
            scratch_updated: Vec::new(),
        }
    }

    pub fn stride(&self) -> usize {
        self.stride
    }
    pub fn chunk_slots(&self) -> usize {
        self.chunk_slots
    }
    pub fn capacity_slots(&self) -> usize {
        self.capacity_slots
    }
    pub fn len(&self) -> usize {
        self.next_slot - self.free_list.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Owner-only: allocate a slot, growing by one chunk if needed. Returns
    /// a slot index whose address is stable for the arena's lifetime.
    pub fn allocate(&mut self) -> usize {
        if let Some(slot) = self.free_list.pop() {
            return slot;
        }
        if self.next_slot == self.capacity_slots {
            self.grow_one_chunk();
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        slot
    }

    /// Owner-only: return a slot to the free-list. Its bytes/version persist
    /// (a later reuse simply bumps the version, which the reader detects).
    pub fn free(&mut self, slot: usize) {
        debug_assert!(slot < self.next_slot);
        self.free_list.push(slot);
    }

    fn grow_one_chunk(&mut self) {
        assert!(
            self.chunks.len() < self.max_chunks,
            "SharedArena exceeded max_chunks ({})",
            self.max_chunks
        );
        let values = vec![0u8; self.chunk_slots * self.stride].into_boxed_slice();
        let versions = (0..self.chunk_slots)
            .map(|_| AtomicU32::new(0))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self.chunks.push(Chunk { values, versions });
        self.capacity_slots += self.chunk_slots;
        self.last_seen.resize(self.capacity_slots, 0);
        self.mirror.resize(self.capacity_slots * self.stride, 0);
    }

    #[inline]
    fn locate(&self, slot: usize) -> (usize, usize) {
        (slot / self.chunk_slots, slot % self.chunk_slots)
    }

    /// In-process foreign-style write (used in tests / single-thread): bump
    /// the seqlock, write bytes, set the chunk dirty bit. `bytes.len()` must
    /// equal the stride.
    pub fn write_value(&self, slot: usize, bytes: &[u8]) {
        debug_assert_eq!(bytes.len(), self.stride);
        let (ci, si) = self.locate(slot);
        let chunk = &self.chunks[ci];
        let version = &chunk.versions[si];
        let odd = seqlock::begin_write(version);
        // SAFETY: single writer per slot; the seqlock orders this against
        // the reader, which detects and discards any torn read.
        unsafe {
            let dst = chunk.values.as_ptr().add(si * self.stride) as *mut u8;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, self.stride);
        }
        seqlock::end_write(version, odd);
        self.dirty.mark(ci);
    }

    /// Raw addresses for binding a foreign (cross-thread) writer to `slot`.
    pub fn slot_binding(&self, slot: usize) -> SlotBinding {
        let (ci, si) = self.locate(slot);
        let chunk = &self.chunks[ci];
        SlotBinding {
            value_addr: unsafe { chunk.values.as_ptr().add(si * self.stride) } as usize,
            version_addr: &chunk.versions[si] as *const AtomicU32 as usize,
            chunk: ci,
        }
    }

    /// Base address of the (address-stable) dirty bitmap.
    pub fn dirty_words_addr(&self) -> usize {
        self.dirty.words_addr()
    }

    /// The contiguous `slot * stride` snapshot of accepted values. This is
    /// the render-private mirror the GPU uploader reads.
    pub fn mirror(&self) -> &[u8] {
        &self.mirror
    }

    /// Reader: descend the dirty chunks, seqlock-read each touched slot into
    /// the private mirror, and return coalesced `(offset, len)` ranges for
    /// the slots that took a fresh value. Torn slots reuse the previous
    /// mirror value and re-arm their chunk for next frame.
    pub fn descend(&mut self) -> DescendResult {
        let mut chunks = std::mem::take(&mut self.scratch_chunks);
        let mut updated = std::mem::take(&mut self.scratch_updated);
        chunks.clear();
        updated.clear();
        self.dirty.drain_into(&mut chunks);

        let mut torn = 0usize;
        for &ci in &chunks {
            if ci >= self.chunks.len() {
                continue;
            }
            let mut chunk_had_torn = false;
            let base_slot = ci * self.chunk_slots;
            for si in 0..self.chunk_slots {
                let slot = base_slot + si;
                if slot >= self.next_slot {
                    break;
                }
                let version = &self.chunks[ci].versions[si];
                let last = self.last_seen[slot];
                // Copy into scratch first; commit to the mirror only on a
                // clean (non-torn) read.
                let src = &self.chunks[ci].values[si * self.stride..(si + 1) * self.stride];
                let scratch = &mut self.scratch_slot;
                let outcome = seqlock::read(version, last, || {
                    scratch.copy_from_slice(src);
                });
                match outcome {
                    SlotRead::Unchanged => {}
                    SlotRead::Updated { version } => {
                        self.last_seen[slot] = version;
                        let off = slot * self.stride;
                        self.mirror[off..off + self.stride].copy_from_slice(&self.scratch_slot);
                        updated.push(slot);
                    }
                    SlotRead::Torn => {
                        torn += 1;
                        chunk_had_torn = true;
                    }
                }
            }
            // A torn read means a writer was mid-flight; re-arm so the next
            // descend retries (the bit was cleared by drain).
            if chunk_had_torn {
                self.dirty.mark(ci);
            }
        }

        let ranges = self.coalesce(&mut updated);

        // Return scratch for reuse.
        self.scratch_chunks = chunks;
        self.scratch_updated = updated;

        DescendResult {
            ranges,
            updated: self.scratch_updated.len(),
            torn,
        }
    }

    /// Coalesce updated slot indices into `(byte_offset, len)` runs.
    fn coalesce(&self, updated: &mut [usize]) -> Vec<(usize, usize)> {
        if updated.is_empty() {
            return Vec::new();
        }
        updated.sort_unstable();
        let mut ranges = Vec::new();
        let mut run_start = updated[0];
        let mut run_end = updated[0]; // inclusive
        for &slot in &updated[1..] {
            if slot == run_end + 1 {
                run_end = slot;
            } else {
                ranges.push((
                    run_start * self.stride,
                    (run_end - run_start + 1) * self.stride,
                ));
                run_start = slot;
                run_end = slot;
            }
        }
        ranges.push((
            run_start * self.stride,
            (run_end - run_start + 1) * self.stride,
        ));
        ranges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIDE: usize = 64; // a Mat4

    fn ramp(seed: u8) -> Vec<u8> {
        (0..STRIDE as u8).map(|i| i.wrapping_add(seed)).collect()
    }

    #[test]
    fn seqlock_odd_even_cycle() {
        let v = AtomicU32::new(0);
        let odd = seqlock::begin_write(&v);
        assert_eq!(odd, 1);
        assert_eq!(v.load(Ordering::Relaxed) & 1, 1, "version odd during write");
        seqlock::end_write(&v, odd);
        assert_eq!(v.load(Ordering::Relaxed), 2);
        assert_eq!(v.load(Ordering::Relaxed) & 1, 0, "version even after write");
    }

    #[test]
    fn seqlock_read_unchanged_updated_clean() {
        // Even version equal to last-seen -> Unchanged.
        let v = AtomicU32::new(4);
        assert_eq!(seqlock::read(&v, 4, || {}), SlotRead::Unchanged);
        // Even version != last-seen, no interleave -> Updated.
        assert_eq!(
            seqlock::read(&v, 2, || {}),
            SlotRead::Updated { version: 4 }
        );
    }

    #[test]
    fn seqlock_read_detects_odd_start() {
        let v = AtomicU32::new(3); // odd: writer mid-flight
        assert_eq!(seqlock::read(&v, 0, || {}), SlotRead::Torn);
    }

    #[test]
    fn seqlock_read_detects_interleaved_write() {
        // Writer flips the version DURING the value copy -> torn.
        let v = AtomicU32::new(2);
        let r = seqlock::read(&v, 0, || {
            v.store(3, Ordering::Relaxed); // writer goes odd mid-read
        });
        assert_eq!(r, SlotRead::Torn);

        // Writer completes a full new write during the copy -> still torn
        // (version moved), so the reader retries rather than accepting a
        // possibly half-written value.
        let v = AtomicU32::new(2);
        let r = seqlock::read(&v, 0, || {
            v.store(4, Ordering::Relaxed);
        });
        assert_eq!(r, SlotRead::Torn);
    }

    #[test]
    fn dirty_bitmap_mark_and_drain() {
        let bm = DirtyBitmap::new(100);
        bm.mark(0);
        bm.mark(5);
        bm.mark(63);
        assert!(bm.is_marked(5));
        let mut out = Vec::new();
        bm.drain_into(&mut out);
        assert_eq!(out, vec![0, 5, 63]);
        assert!(!bm.is_marked(5), "drain clears bits");
        out.clear();
        bm.drain_into(&mut out);
        assert!(out.is_empty(), "second drain is empty");
    }

    #[test]
    fn write_then_descend_roundtrip() {
        let mut a = SharedArena::new(STRIDE, 4, 16);
        let s0 = a.allocate();
        let s1 = a.allocate();
        let s2 = a.allocate();
        assert_eq!((s0, s1, s2), (0, 1, 2));

        a.write_value(s0, &ramp(10));
        a.write_value(s1, &ramp(20));
        a.write_value(s2, &ramp(30));

        let r = a.descend();
        assert_eq!(r.torn, 0);
        assert_eq!(r.updated, 3);
        // Contiguous slots 0..=2 coalesce into a single range.
        assert_eq!(r.ranges, vec![(0, 3 * STRIDE)]);
        assert_eq!(&a.mirror()[0..STRIDE], &ramp(10)[..]);
        assert_eq!(&a.mirror()[STRIDE..2 * STRIDE], &ramp(20)[..]);
        assert_eq!(&a.mirror()[2 * STRIDE..3 * STRIDE], &ramp(30)[..]);

        // Nothing dirty now.
        let r2 = a.descend();
        assert!(r2.ranges.is_empty());
        assert_eq!(r2.updated, 0);
    }

    #[test]
    fn descend_coalesces_noncontiguous_runs() {
        let mut a = SharedArena::new(STRIDE, 8, 16); // one chunk holds 0..=7
        for _ in 0..8 {
            a.allocate();
        }
        // Touch slots 0,1,2 and 5 -> two runs.
        a.write_value(0, &ramp(1));
        a.write_value(1, &ramp(2));
        a.write_value(2, &ramp(3));
        a.write_value(5, &ramp(4));
        let r = a.descend();
        assert_eq!(r.updated, 4);
        assert_eq!(r.ranges, vec![(0, 3 * STRIDE), (5 * STRIDE, STRIDE)]);
    }

    #[test]
    fn dirty_scan_tracks_touched_chunks_not_total() {
        // Many chunks, but only one is written -> descend visits one chunk.
        let mut a = SharedArena::new(STRIDE, 4, 64);
        for _ in 0..200 {
            a.allocate(); // 50 chunks
        }
        a.write_value(100, &ramp(7)); // chunk 25
        let mut probe = Vec::new();
        // The bitmap reports exactly one dirty chunk before descending.
        a.dirty.drain_into(&mut probe);
        assert_eq!(probe, vec![25]);
        // Re-mark (we drained it) and let descend do the real read.
        a.dirty.mark(25);
        let r = a.descend();
        assert_eq!(r.updated, 1);
        assert_eq!(r.ranges, vec![(100 * STRIDE, STRIDE)]);
    }

    #[test]
    fn stable_addressing_across_grow() {
        let mut a = SharedArena::new(STRIDE, 4, 16);
        // First chunk.
        for _ in 0..4 {
            a.allocate();
        }
        let b0 = a.slot_binding(0);
        let b3 = a.slot_binding(3);
        let dirty_addr = a.dirty_words_addr();
        // Grow several more chunks.
        for _ in 0..20 {
            a.allocate();
        }
        // Earlier bindings + the bitmap address are unchanged after growth.
        assert_eq!(a.slot_binding(0), b0, "slot 0 address moved on grow");
        assert_eq!(a.slot_binding(3), b3, "slot 3 address moved on grow");
        assert_eq!(a.dirty_words_addr(), dirty_addr, "bitmap address moved");
    }

    #[test]
    fn foreign_write_matches_in_process_write() {
        // Drive the unsafe raw-address path and confirm the reader sees it.
        let mut a = SharedArena::new(STRIDE, 4, 16);
        for _ in 0..4 {
            a.allocate();
        }
        let binding = a.slot_binding(2);
        let dirty_addr = a.dirty_words_addr();
        unsafe {
            foreign_write(binding, dirty_addr, &ramp(42));
        }
        let r = a.descend();
        assert_eq!(r.updated, 1);
        assert_eq!(r.torn, 0);
        assert_eq!(r.ranges, vec![(2 * STRIDE, STRIDE)]);
        assert_eq!(&a.mirror()[2 * STRIDE..3 * STRIDE], &ramp(42)[..]);
    }

    #[test]
    fn allocate_reuses_freed_slots() {
        let mut a = SharedArena::new(STRIDE, 4, 16);
        let s0 = a.allocate();
        let s1 = a.allocate();
        assert_eq!(a.len(), 2);
        a.free(s0);
        assert_eq!(a.len(), 1);
        let s2 = a.allocate();
        assert_eq!(s2, s0, "freed slot is reused");
        assert_ne!(s1, s2);
        assert_eq!(a.len(), 2);
    }
}
