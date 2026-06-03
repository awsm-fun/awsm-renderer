//! Dynamic storage buffer utilities.

use slotmap::{Key, SecondaryMap};
use thiserror::Error;

/// Errors from [`DynamicStorageBuffer`] allocation.
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicStorageBufferError {
    #[error("buffer capacity overflow (requested size exceeds platform limit)")]
    CapacityOverflow,
}

/// Dynamic buffer for variable-size allocations using buddy memory allocation.
///
/// This buffer supports allocations of arbitrary sizes, automatically rounded to
/// power-of-two for efficient buddy allocation. Ideal for heterogeneous data.
///
///-------------------------------- PERFORMANCE SUMMARY ------------------------//
///
/// • insert/update/remove:   O(log N) (amortized, ignoring rare growth)
/// • GPU write (per frame):  uploads dirty ranges (full upload when dense)
/// • Resize strategy:        doubles capacity when needed; rebuilds tree
///                           (infrequent pauses)
/// • External fragmentation: none (buddy blocks always coalesce)
/// • Internal fragmentation: ≤ 50% per allocation (due to power-of-two rounding)
/// • Memory overhead:        raw_data.len() rounded up + buddy tree (~2× leaves)
///
/// • Ideal usage:
///    Mixed-size uniform/storage buffer items where predictable performance
///    matters more than perfect memory efficiency, like:
///      - Heterogeneous UBO/SBO payloads (i.e. not all items are the same size)
///      - Variable-sized dynamic allocations (i.e. varying number of items per draw call)
///
/// For example, vertex data that changes per-mesh
///
///----------------------------------------------------------------------------//
///
/// Minimum alloc unit – choose 256 B so every buddy is WebGPU‑aligned.
/// Must be power‑of‑two.
const MIN_BLOCK: usize = 256;

/// This uses "buddy memory allocation" to allow allocations of an arbitrary size
/// that are power‑of‑two aligned. It is a bit more complex and wasteful than the
/// `DynamicFixedBuffer`, but it allows for more flexible allocation sizes
/// with still-excellent performance tradeoffs due to the buddy tree structure.
#[derive(Debug)]
pub struct DynamicStorageBuffer<K: Key, const ZERO: u8 = 0> {
    raw_data: Vec<u8>,
    dirty_ranges: Vec<(usize, usize)>,
    /// Complete binary tree stored as an array where each node
    /// is the size of the *largest* free block in that subtree.
    buddy_tree: Vec<usize>,
    slot_indices: SecondaryMap<K, (usize /*offset*/, usize /*size*/)>,

    // --- GPU side & misc ---
    gpu_buffer_needs_resize: bool,
    label: Option<String>,
    /// The original `initial_bytes` argument passed to [`Self::new`],
    /// kept so that [`Self::clear`] can reset to the same starting size.
    initial_bytes_arg: usize,
}

impl<K: Key, const ZERO: u8> DynamicStorageBuffer<K, ZERO> {
    /// Creates a new dynamic storage buffer.
    pub fn new(initial_bytes: usize, label: Option<String>) -> Self {
        let initial_bytes_arg = initial_bytes;
        // round up to next power‑of‑two multiple of MIN_BLOCK
        let capacity = round_pow2(initial_bytes.max(MIN_BLOCK));

        // buddy tree size: 2 * cap / MIN_BLOCK  – 1  (perfect binary tree)
        let leaves = capacity / MIN_BLOCK;
        let mut buddy_tree = vec![0; 2 * leaves - 1];

        init_full(&mut buddy_tree, 0, capacity);

        // CPU
        let raw_data = vec![ZERO; capacity];

        Self {
            raw_data,
            dirty_ranges: Vec::new(),
            buddy_tree,
            slot_indices: SecondaryMap::new(),
            gpu_buffer_needs_resize: capacity != initial_bytes,
            label,
            initial_bytes_arg,
        }
    }

    /// Resets the buffer to its initial capacity, dropping all allocations.
    ///
    /// After this call the buffer is in the same state as a freshly
    /// constructed one (with the same `initial_bytes` and `label`).
    pub fn clear(&mut self) {
        let fresh = Self::new(self.initial_bytes_arg, self.label.clone());
        *self = fresh;
    }

    /* ------------------------------------------------------------------ */
    /*                    PUBLIC API: update / remove                     */
    /* ------------------------------------------------------------------ */

    /// Updates or inserts data for the given key.
    ///
    /// If the key exists and the new data fits in the existing allocation,
    /// it reuses the same memory. Otherwise, it reallocates.
    ///
    /// Returns the byte offset of the data in the buffer, or an error if the
    /// buffer cannot grow large enough to accommodate the data.
    pub fn update(&mut self, key: K, bytes: &[u8]) -> Result<usize, DynamicStorageBufferError> {
        // remove & reinsert if new size doesn’t fit existing block
        if let Some((off, old_size)) = self.slot_indices.get(key).copied() {
            if bytes.len() <= old_size {
                self.raw_data[off..off + bytes.len()].copy_from_slice(bytes);
                // clear unused tail
                if bytes.len() < old_size {
                    self.raw_data[off + bytes.len()..off + old_size].fill(ZERO);
                }
                self.mark_dirty_range(off, old_size);
                return Ok(off);
            }
            self.remove(key);
        }
        self.insert(key, bytes)
    }

    /// Updates existing data using a callback, without reallocation.
    ///
    /// # Panics
    /// Panics if the key doesn't exist.
    pub fn update_with_unchecked(&mut self, key: K, f: impl FnOnce(usize, &mut [u8])) {
        match self.slot_indices.get(key) {
            Some((off, size)) => {
                f(*off, &mut self.raw_data[*off..*off + *size]);
                self.mark_dirty_range(*off, *size);
            }
            None => {
                panic!("Key {key:?} not found in DynamicBuddyBuffer");
            }
        }
    }

    // Use update() instead; this always inserts a new allocation.
    fn insert(&mut self, key: K, bytes: &[u8]) -> Result<usize, DynamicStorageBufferError> {
        let req = bytes
            .len()
            .max(MIN_BLOCK)
            .checked_next_power_of_two()
            .ok_or(DynamicStorageBufferError::CapacityOverflow)?;
        let off = match self.alloc(req) {
            Some(off) => off,
            None => {
                // grow buffer & tree, then retry
                self.grow(req)?;
                self.alloc(req).ok_or_else(|| {
                    tracing::error!(
                        "buddy alloc failed after grow: \
                         requested={req} capacity={} used={} label={:?}",
                        self.raw_data.len(),
                        self.used_size(),
                        self.label,
                    );
                    DynamicStorageBufferError::CapacityOverflow
                })?
            }
        };
        self.raw_data[off..off + bytes.len()].copy_from_slice(bytes);
        self.slot_indices.insert(key, (off, req));
        self.mark_dirty_range(off, req);

        Ok(off)
    }

    /// Removes a key and frees its allocation.
    pub fn remove(&mut self, key: K) {
        if let Some((off, size)) = self.slot_indices.remove(key) {
            self.raw_data[off..off + size].fill(ZERO);
            self.mark_dirty_range(off, size);
            self.free(off, size);
        }
    }

    /// Returns the total size of all active allocations (excluding fragmentation).
    pub fn used_size(&self) -> usize {
        self.slot_indices
            .values()
            .map(|(_, size)| *size)
            .sum::<usize>()
    }

    /// Gets an immutable view into the slice
    pub fn get(&self, key: K) -> Option<&[u8]> {
        let (off, size) = self.slot_indices.get(key)?;
        Some(&self.raw_data[*off..*off + *size])
    }

    /// Returns the allocated block size (in bytes) for the given key.
    pub fn allocated_size(&self, key: K) -> Option<usize> {
        self.slot_indices.get(key).map(|(_, size)| *size)
    }

    /* ------------------------------------------------------------------ */
    /*                GPU write                                           */
    /* ------------------------------------------------------------------ */

    /// Returns the full raw buffer slice.
    pub fn raw_slice(&self) -> &[u8] {
        &self.raw_data
    }

    /// Takes and clears dirty ranges.
    pub fn take_dirty_ranges(&mut self) -> Vec<(usize, usize)> {
        std::mem::take(&mut self.dirty_ranges)
    }

    /// Clears dirty ranges without returning them.
    pub fn clear_dirty_ranges(&mut self) {
        self.dirty_ranges.clear();
    }

    /// Returns the new size if the GPU buffer needs resizing.
    pub fn take_gpu_needs_resize(&mut self) -> Option<usize> {
        let size = match self.gpu_buffer_needs_resize {
            true => Some(self.raw_data.len()),
            false => None,
        };

        self.gpu_buffer_needs_resize = false;

        size
    }

    fn mark_dirty_range(&mut self, offset: usize, size: usize) {
        if size == 0 || self.raw_data.is_empty() || offset >= self.raw_data.len() {
            return;
        }

        let mut start = offset;
        let mut end = offset.saturating_add(size).min(self.raw_data.len());

        // WebGPU writeBuffer offsets/sizes must be 4-byte aligned.
        start &= !3;
        end = ((end + 3) & !3).min(self.raw_data.len());

        if start < end {
            self.dirty_ranges.push((start, end - start));
        }
    }

    /* ------------------------------------------------------------------ */
    /*                  Buddy tree helpers                                */
    /* ------------------------------------------------------------------ */

    /// Allocate a block of exactly `req` bytes (power‑of‑two, ≥ MIN_BLOCK).
    fn alloc(&mut self, req: usize) -> Option<usize> {
        if req > self.buddy_tree[0] {
            return None;
        }

        let mut idx = 0usize; // start at root
        let mut size = self.raw_data.len(); // current block size

        while size > req {
            let half = size / 2;
            let left = idx * 2 + 1;
            // choose the child that can still satisfy the request
            idx = if self.buddy_tree[left] >= req {
                left
            } else {
                left + 1
            };
            size = half;
        }

        // `idx` now points to the block we want
        self.buddy_tree[idx] = 0;
        fix_parents(&mut self.buddy_tree, idx);

        Some(index_to_offset(idx, self.raw_data.len() / MIN_BLOCK))
    }

    /// Marks a previously‑allocated block `[offset , offset+size)` as free.
    ///
    /// `offset` **must** be the same value returned by `alloc`, and
    /// `size` **must** be a power‑of‑two ≥ MIN_BLOCK (already true because it
    /// comes from `self.slot_indices`).
    fn free(&mut self, offset: usize, size: usize) {
        let leaves = self.raw_data.len() / MIN_BLOCK;

        // ── 1. find the tree‑node that owns this exact block ────────────────
        let mut idx = offset_to_index(offset, leaves); // start at leaf
        let mut blk = MIN_BLOCK; // leaf block size

        while blk < size {
            // climb until we reach the right level
            idx = (idx - 1) >> 1;
            blk <<= 1;
        }
        self.buddy_tree[idx] = blk; // mark that node free

        // ── 2. bubble upward, merging buddies when BOTH are equally free ────
        while idx != 0 {
            let parent = (idx - 1) >> 1;
            let left = parent * 2 + 1;
            let right = left + 1;

            let merged = self.buddy_tree[left] == blk && self.buddy_tree[right] == blk;

            let new_val = if merged {
                blk << 1 // buddies coalesce → parent block size
            } else {
                self.buddy_tree[left].max(self.buddy_tree[right])
            };

            if self.buddy_tree[parent] == new_val {
                break; // nothing changed ⇒ done
            }
            self.buddy_tree[parent] = new_val;

            if merged {
                idx = parent; // continue trying to merge upward
                blk <<= 1;
            } else {
                break; // no merge ⇒ parents are already correct
            }
        }
    }

    /// The largest power-of-two capacity that fits in a `Vec<u8>`.
    /// On 32-bit (wasm32): 2^30 = 1 GiB.  On 64-bit: 2^62 (~4 EiB).
    const MAX_BUDDY_CAPACITY: usize = 1usize << (usize::BITS - 2);

    fn grow(&mut self, min_extra: usize) -> Result<(), DynamicStorageBufferError> {
        let old_cap = self.raw_data.len();

        if old_cap >= Self::MAX_BUDDY_CAPACITY {
            tracing::error!(
                "DynamicStorageBuffer at platform ceiling: \
                 capacity={old_cap} used={} max={} label={:?}",
                self.used_size(),
                Self::MAX_BUDDY_CAPACITY,
                self.label,
            );
            return Err(DynamicStorageBufferError::CapacityOverflow);
        }

        let needed = old_cap
            .checked_add(min_extra)
            .ok_or(DynamicStorageBufferError::CapacityOverflow)?;

        // Double until we meet the requirement, but cap at the platform ceiling.
        let mut new_cap = old_cap
            .checked_mul(2)
            .map(|c| c.min(Self::MAX_BUDDY_CAPACITY))
            .ok_or(DynamicStorageBufferError::CapacityOverflow)?;

        while new_cap < needed {
            if new_cap >= Self::MAX_BUDDY_CAPACITY {
                tracing::error!(
                    "DynamicStorageBuffer cannot grow enough: \
                     needed={needed} max={} used={} label={:?}",
                    Self::MAX_BUDDY_CAPACITY,
                    self.used_size(),
                    self.label,
                );
                return Err(DynamicStorageBufferError::CapacityOverflow);
            }
            new_cap = new_cap
                .checked_mul(2)
                .map(|c| c.min(Self::MAX_BUDDY_CAPACITY))
                .ok_or(DynamicStorageBufferError::CapacityOverflow)?;
        }

        self.raw_data.resize(new_cap, ZERO);
        self.gpu_buffer_needs_resize = true;

        // rebuild a new perfect tree
        let leaves = new_cap / MIN_BLOCK;
        self.buddy_tree.clear();
        self.buddy_tree.resize(2 * leaves - 1, 0);
        init_full(&mut self.buddy_tree, 0, new_cap);

        // re‑insert existing allocations so they become *used* leaves
        for (offset, size) in self.slot_indices.values().cloned() {
            mark_used(&mut self.buddy_tree, self.raw_data.len(), offset, size);
        }

        Ok(())
    }

    /* ---------- tiny query helpers (unchanged APIs) -------------------- */

    /// Returns the byte offset for a key.
    pub fn offset(&self, key: K) -> Option<usize> {
        self.slot_indices.get(key).map(|&(off, _)| off)
    }
    /// Returns the allocated size for a key.
    pub fn size(&self, key: K) -> Option<usize> {
        self.slot_indices.get(key).map(|&(_, sz)| sz)
    }

    /// Returns the number of currently allocated keys
    pub fn len(&self) -> usize {
        self.slot_indices.len()
    }

    /// Returns true if no keys are allocated
    pub fn is_empty(&self) -> bool {
        self.slot_indices.is_empty()
    }

    /// Returns the total buffer capacity in bytes
    pub fn capacity(&self) -> usize {
        self.raw_data.len()
    }

    /// Checks if a key exists in the buffer
    pub fn contains_key(&self, key: K) -> bool {
        self.slot_indices.contains_key(key)
    }

    /// Returns an iterator over all keys
    pub fn keys(&self) -> impl Iterator<Item = K> + '_ {
        self.slot_indices.keys()
    }
}

/// Recursively initialise the subtree rooted at `node` so that every
/// entry stores the size of the largest free block in that subtree.
/// `size` is the total byte size represented by that node.
fn init_full(tree: &mut [usize], node: usize, size: usize) {
    tree[node] = size;
    if size > MIN_BLOCK {
        let half = size / 2;
        let left = node * 2 + 1;
        let right = left + 1;
        init_full(tree, left, half);
        init_full(tree, right, half);
    }
}

#[inline]
fn fix_parents(buddy_tree: &mut [usize], mut idx: usize) {
    while idx != 0 {
        let parent = (idx - 1) >> 1;
        let left = parent * 2 + 1;
        let right = left + 1;
        let new_val = buddy_tree[left].max(buddy_tree[right]);
        if buddy_tree[parent] == new_val {
            break;
        }
        buddy_tree[parent] = new_val;
        idx = parent;
    }
}

fn mark_used(buddy_tree: &mut [usize], raw_data_len: usize, offset: usize, size: usize) {
    let leaves = raw_data_len / MIN_BLOCK;
    let mut idx = offset_to_index(offset, leaves);
    let mut sz = MIN_BLOCK;
    while sz < size {
        idx = (idx - 1) >> 1;
        sz <<= 1;
    }
    buddy_tree[idx] = 0;
    fix_parents(buddy_tree, idx);
}

/* ---- index/offset helpers & math utils ---------------------------- */

#[inline]
fn round_pow2(n: usize) -> usize {
    n.next_power_of_two().max(MIN_BLOCK)
}
#[inline]
fn index_to_offset(mut idx: usize, leaves: usize) -> usize {
    // walk to the left‑most leaf of this subtree
    while idx < leaves - 1 {
        idx = idx * 2 + 1;
    }
    let leaf_idx = idx + 1 - leaves;
    leaf_idx * MIN_BLOCK
}
#[inline]
fn offset_to_index(off: usize, leaves: usize) -> usize {
    leaves - 1 + off / MIN_BLOCK
}

#[cfg(test)]
mod test {
    use super::*;
    use slotmap::SlotMap;

    type TestKey = slotmap::DefaultKey;

    fn create_test_buffer() -> DynamicStorageBuffer<TestKey> {
        DynamicStorageBuffer::new(
            1024, // initial capacity of 1024 bytes
            Some("test_buffer".to_string()),
        )
    }

    fn create_keys() -> (SlotMap<TestKey, ()>, TestKey, TestKey, TestKey) {
        let mut key_map = SlotMap::new();
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());
        let key3 = key_map.insert(());
        (key_map, key1, key2, key3)
    }

    #[test]
    fn test_new_buffer_initialization() {
        let buffer = create_test_buffer();

        // Initial capacity should be rounded to power of 2
        assert_eq!(buffer.raw_data.len(), 1024);

        // All data should initially be zeros
        assert!(buffer.raw_data.iter().all(|&b| b == 0));

        // No keys should be assigned
        assert_eq!(buffer.slot_indices.len(), 0);

        // Root should have full capacity available
        assert_eq!(buffer.buddy_tree[0], 1024);
    }

    #[test]
    fn test_insert_single_item() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        let test_data = b"hello world test data";
        let offset = buffer.update(key1, test_data).unwrap();

        // Should have allocated space
        assert!(buffer.slot_indices.contains_key(key1));

        // Offset should be valid
        assert_eq!(offset, 0); // First allocation should be at offset 0

        // Check that data was written correctly
        assert_eq!(
            &buffer.raw_data[offset..offset + test_data.len()],
            test_data
        );

        // Verify the allocation size is power of 2 and >= MIN_BLOCK
        let size = buffer.size(key1).unwrap();
        assert!(size.is_power_of_two());
        assert!(size >= MIN_BLOCK);
    }

    #[test]
    fn test_insert_multiple_items() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, _) = create_keys();

        let data1 = b"first data block";
        let data2 = b"second data block with more content";

        let offset1 = buffer.update(key1, data1).unwrap();
        let offset2 = buffer.update(key2, data2).unwrap();

        // Both items should be stored
        assert!(buffer.slot_indices.contains_key(key1));
        assert!(buffer.slot_indices.contains_key(key2));

        // Offsets should be different
        assert_ne!(offset1, offset2);

        // Verify data integrity
        assert_eq!(&buffer.raw_data[offset1..offset1 + data1.len()], data1);
        assert_eq!(&buffer.raw_data[offset2..offset2 + data2.len()], data2);
    }

    #[test]
    fn test_update_existing_item_same_size() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Insert initial data
        let initial_data = b"initial data content";
        let initial_offset = buffer.update(key1, initial_data).unwrap();
        let initial_size = buffer.size(key1).unwrap();

        // Update with data that fits in same block
        let updated_data = b"updated data content";
        let updated_offset = buffer.update(key1, updated_data).unwrap();

        // Should reuse same allocation
        assert_eq!(initial_offset, updated_offset);
        assert_eq!(buffer.size(key1).unwrap(), initial_size);

        // Data should be updated
        assert_eq!(
            &buffer.raw_data[updated_offset..updated_offset + updated_data.len()],
            updated_data
        );
    }

    #[test]
    fn test_update_existing_item_larger_size() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Insert small data
        let small_data = vec![1u8; 10];
        buffer.update(key1, &small_data).unwrap();

        // Update with larger data that needs reallocation
        let large_data = vec![2u8; 300];
        let new_offset = buffer.update(key1, &large_data).unwrap();

        // Should have reallocated
        let new_size = buffer.size(key1).unwrap();
        assert!(new_size >= 512); // Next power of 2 after 300

        // Data should be correct
        assert_eq!(
            &buffer.raw_data[new_offset..new_offset + large_data.len()],
            &large_data[..]
        );
    }

    #[test]
    fn test_remove_item() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, _) = create_keys();

        // Insert two items
        let data1 = b"data one";
        let data2 = b"data two";

        let offset1 = buffer.update(key1, data1).unwrap();
        buffer.update(key2, data2).unwrap();

        let size1 = buffer.size(key1).unwrap();

        // Remove first item
        buffer.remove(key1);

        // Key should no longer exist
        assert_eq!(buffer.offset(key1), None);
        assert_eq!(buffer.size(key1), None);
        assert!(!buffer.slot_indices.contains_key(key1));

        // Data should be zeroed out
        assert!(buffer.raw_data[offset1..offset1 + size1]
            .iter()
            .all(|&b| b == 0));

        // Second key should still work
        assert!(buffer.offset(key2).is_some());
    }

    #[test]
    fn test_buddy_allocation_reuse() {
        let mut buffer = create_test_buffer();
        let (_key_map, key1, key2, key3) = create_keys();

        // Allocate and free to test buddy system
        let data = vec![1u8; 100];

        buffer.update(key1, &data).unwrap();
        buffer.update(key2, &data).unwrap();

        let offset1 = buffer.offset(key1).unwrap();

        // Remove first item
        buffer.remove(key1);

        // New allocation should potentially reuse the freed space
        buffer.update(key3, &data).unwrap();
        let offset3 = buffer.offset(key3).unwrap();

        // Should reuse the freed block
        assert_eq!(offset1, offset3);
    }

    #[test]
    fn test_buffer_growth() {
        let mut buffer: DynamicStorageBuffer<TestKey> = DynamicStorageBuffer::new(
            512, // Start small
            Some("growth_test".to_string()),
        );

        let (mut key_map, _, _, _) = create_keys();

        // Fill buffer beyond initial capacity
        let large_data = vec![42u8; 400];
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());

        buffer.update(key1, &large_data).unwrap();

        let initial_capacity = buffer.raw_data.len();

        // This should trigger growth
        buffer.update(key2, &large_data).unwrap();

        // Buffer should have grown
        assert!(buffer.raw_data.len() > initial_capacity);
        assert!(buffer.raw_data.len().is_power_of_two());

        // Both allocations should be valid
        assert!(buffer.offset(key1).is_some());
        assert!(buffer.offset(key2).is_some());
    }

    #[test]
    fn test_gpu_resize_flag() {
        let mut buffer: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(256, Some("resize_flag_test".to_string()));

        let (_, key1, key2, _) = create_keys();

        // Initially no resize needed (unless initial size was adjusted)
        let _initial_flag = buffer.take_gpu_needs_resize();

        // Small allocations shouldn't trigger resize
        buffer.update(key1, b"small").unwrap();
        assert_eq!(buffer.take_gpu_needs_resize(), None);

        // Large allocation should trigger growth
        let large_data = vec![1u8; 200];
        buffer.update(key2, &large_data).unwrap();

        // Should indicate resize needed
        let resize_size = buffer.take_gpu_needs_resize();
        assert!(resize_size.is_some());

        // Flag should be reset after taking
        assert_eq!(buffer.take_gpu_needs_resize(), None);
    }

    #[test]
    fn test_power_of_two_rounding() {
        let mut buffer = create_test_buffer();
        let mut key_map = SlotMap::new();

        // Test various sizes to ensure power-of-2 rounding
        let test_sizes = vec![1, 15, 16, 17, 100, 255, 256, 257, 500];

        for size in test_sizes {
            let key = key_map.insert(());
            let data = vec![0xAA; size];

            buffer.update(key, &data).unwrap();

            let allocated_size = buffer.size(key).unwrap();
            assert!(allocated_size.is_power_of_two());
            assert!(allocated_size >= size);
            assert!(allocated_size >= MIN_BLOCK);
        }
    }

    #[test]
    fn test_buddy_coalescing() {
        let mut buffer = create_test_buffer();
        let (mut key_map, _, _, _) = create_keys();

        // Allocate adjacent blocks
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());

        let data = vec![1u8; MIN_BLOCK];

        buffer.update(key1, &data).unwrap();
        buffer.update(key2, &data).unwrap();

        // Remove both to test if buddies coalesce
        buffer.remove(key1);
        buffer.remove(key2);

        // Allocate larger block that should use coalesced space
        let key3 = key_map.insert(());
        let large_data = vec![2u8; MIN_BLOCK * 2];

        let offset = buffer.update(key3, &large_data).unwrap();

        // Should be able to allocate at beginning (coalesced buddies)
        assert_eq!(offset, 0);
    }

    #[test]
    fn test_update_with_unchecked() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Insert initial data
        let initial_data = vec![0u8; 100];
        buffer.update(key1, &initial_data).unwrap();

        // Update using the callback
        buffer.update_with_unchecked(key1, |offset, data| {
            assert_eq!(offset, 0); // First allocation at offset 0
            assert!(data.len() >= 100); // Should have at least requested size

            // Modify the data
            data[0..4].copy_from_slice(b"TEST");
        });

        // Verify modification
        let offset = buffer.offset(key1).unwrap();
        assert_eq!(&buffer.raw_data[offset..offset + 4], b"TEST");
    }

    #[test]
    #[should_panic(expected = "not found")]
    fn test_update_with_unchecked_missing_key() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Try to update non-existent key
        buffer.update_with_unchecked(key1, |_, _| {});
    }

    #[test]
    fn test_zero_value_variants() {
        // Test with default zero value (0)
        let mut buffer1: DynamicStorageBuffer<TestKey, 0> =
            DynamicStorageBuffer::new(512, Some("zero_buffer".to_string()));

        // Test with custom zero value (0xFF)
        let mut buffer2: DynamicStorageBuffer<TestKey, 0xFF> =
            DynamicStorageBuffer::new(512, Some("ones_buffer".to_string()));

        let (_, key1, key2, _) = create_keys();

        // Add and remove items to test zero fill behavior
        buffer1.update(key1, b"testdata").unwrap();
        buffer2.update(key2, b"testdata").unwrap();

        let offset1 = buffer1.offset(key1).unwrap();
        let size1 = buffer1.size(key1).unwrap();
        let offset2 = buffer2.offset(key2).unwrap();
        let size2 = buffer2.size(key2).unwrap();

        buffer1.remove(key1);
        buffer2.remove(key2);

        // Check that removed blocks are filled with correct zero value
        assert!(buffer1.raw_data[offset1..offset1 + size1]
            .iter()
            .all(|&b| b == 0));
        assert!(buffer2.raw_data[offset2..offset2 + size2]
            .iter()
            .all(|&b| b == 0xFF));
    }

    #[test]
    fn test_large_scale_operations() {
        let mut buffer: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(1024, Some("stress_test".to_string()));

        let mut key_map = SlotMap::new();
        let mut keys = Vec::new();

        // Insert many items of varying sizes
        for i in 0..50 {
            let key = key_map.insert(());
            keys.push(key);

            // Vary the size
            let size = 10 + (i * 7) % 200;
            let data = vec![(i % 256) as u8; size];

            buffer.update(key, &data).unwrap();
        }

        // Verify all items are accessible
        for (i, &key) in keys.iter().enumerate() {
            assert!(buffer.offset(key).is_some());
            assert!(buffer.size(key).is_some());

            // Verify data integrity
            let offset = buffer.offset(key).unwrap();
            let size = 10 + (i * 7) % 200;
            let expected_byte = (i % 256) as u8;

            for j in 0..size {
                assert_eq!(buffer.raw_data[offset + j], expected_byte);
            }
        }

        // Remove every other item
        for (i, &key) in keys.iter().enumerate() {
            if i % 2 == 0 {
                buffer.remove(key);
            }
        }

        // Add new items that should reuse freed space
        for i in 100..125 {
            let key = key_map.insert(());
            let size = 15 + (i * 11) % 150;
            let data = vec![(i % 256) as u8; size];

            buffer.update(key, &data).unwrap();
        }
    }

    #[test]
    fn test_raw_slice_access() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Initially should be all zeros
        let raw = buffer.raw_slice();
        assert_eq!(raw.len(), 1024);

        // Add data
        let test_data = b"test data content here";
        buffer.update(key1, test_data).unwrap();

        // Raw slice should reflect the changes
        let raw = buffer.raw_slice();
        let offset = buffer.offset(key1).unwrap();
        assert_eq!(&raw[offset..offset + test_data.len()], test_data);
    }

    #[test]
    fn test_used_size_tracking() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, key3) = create_keys();

        // Initially no space used
        assert_eq!(buffer.used_size(), 0);

        // Add items and track used size
        buffer.update(key1, &[1u8; 100]).unwrap();
        let size1 = buffer.size(key1).unwrap();
        assert_eq!(buffer.used_size(), size1);

        buffer.update(key2, &[2u8; 200]).unwrap();
        let size2 = buffer.size(key2).unwrap();
        assert_eq!(buffer.used_size(), size1 + size2);

        buffer.update(key3, &[3u8; 50]).unwrap();
        let size3 = buffer.size(key3).unwrap();
        assert_eq!(buffer.used_size(), size1 + size2 + size3);

        // Remove an item
        buffer.remove(key2);
        assert_eq!(buffer.used_size(), size1 + size3);
    }

    #[test]
    fn test_minimum_block_size() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Allocate very small data
        let tiny_data = b"x";
        buffer.update(key1, tiny_data).unwrap();

        // Should still allocate at least MIN_BLOCK
        let size = buffer.size(key1).unwrap();
        assert_eq!(size, MIN_BLOCK);
    }

    #[test]
    fn test_buddy_tree_operations() {
        let mut buffer: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(1024, Some("tree_test".to_string()));

        let mut key_map = SlotMap::new();

        // Perform various operations
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());
        let key3 = key_map.insert(());

        buffer.update(key1, &[1u8; 100]).unwrap();
        buffer.update(key2, &[2u8; 200]).unwrap();
        buffer.remove(key1);
        buffer.update(key3, &[3u8; 150]).unwrap();

        // Verify that allocations work correctly and don't overlap
        let offset2 = buffer.offset(key2).unwrap();
        let size2 = buffer.size(key2).unwrap();
        let offset3 = buffer.offset(key3).unwrap();
        let size3 = buffer.size(key3).unwrap();

        // Ensure no overlaps
        assert!(
            offset3 + size3 <= offset2 || offset2 + size2 <= offset3,
            "Allocations overlap: key2=[{}, {}), key3=[{}, {})",
            offset2,
            offset2 + size2,
            offset3,
            offset3 + size3
        );

        // Verify data integrity
        for i in 0..200.min(size2) {
            assert_eq!(buffer.raw_data[offset2 + i], 2u8);
        }
        for i in 0..150.min(size3) {
            assert_eq!(buffer.raw_data[offset3 + i], 3u8);
        }
    }

    #[test]
    fn test_allocation_patterns() {
        let mut buffer: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(2048, Some("pattern_test".to_string()));

        let mut key_map = SlotMap::new();
        let mut keys = Vec::new();

        // Allocate in specific pattern to test buddy algorithm
        // First, fill with small allocations
        for _ in 0..4 {
            let key = key_map.insert(());
            keys.push(key);
            buffer.update(key, &[0xAA; MIN_BLOCK]).unwrap();
        }

        // Remove alternating ones to create fragmentation
        buffer.remove(keys[0]);
        buffer.remove(keys[2]);

        // Try to allocate a larger block
        let key_large = key_map.insert(());
        let large_data = vec![0xBB; MIN_BLOCK * 2];
        let offset = buffer.update(key_large, &large_data).unwrap();

        // Should not be able to use fragmented space at beginning
        assert!(offset >= MIN_BLOCK * 4);
    }

    #[test]
    fn test_grow_with_existing_allocations() {
        let mut buffer: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(512, Some("grow_preserve_test".to_string()));

        let (mut key_map, _, _, _) = create_keys();

        // Make initial allocations
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());

        let data1 = vec![0x11; 100];
        let data2 = vec![0x22; 150];

        let offset1 = buffer.update(key1, &data1).unwrap();
        let offset2 = buffer.update(key2, &data2).unwrap();

        // Force growth
        let key3 = key_map.insert(());
        let large_data = vec![0x33; 400];
        buffer.update(key3, &large_data).unwrap();

        // Original allocations should still be valid
        assert_eq!(buffer.offset(key1), Some(offset1));
        assert_eq!(buffer.offset(key2), Some(offset2));

        // Data should be preserved
        assert_eq!(&buffer.raw_data[offset1..offset1 + data1.len()], &data1[..]);
        assert_eq!(&buffer.raw_data[offset2..offset2 + data2.len()], &data2[..]);
    }

    #[test]
    fn test_initial_size_rounding() {
        // Test that initial size is rounded to power of 2
        let buffer1: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(1000, Some("round_test_1".to_string()));
        assert_eq!(buffer1.raw_data.len(), 1024);

        let buffer2: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(2000, Some("round_test_2".to_string()));
        assert_eq!(buffer2.raw_data.len(), 2048);

        // Test minimum size
        let buffer3: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(10, Some("round_test_3".to_string()));
        assert_eq!(buffer3.raw_data.len(), MIN_BLOCK);
    }

    #[test]
    fn test_offset_and_size_queries() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, _) = create_keys();

        // Test with non-existent key
        assert_eq!(buffer.offset(key1), None);
        assert_eq!(buffer.size(key1), None);

        // Add items
        let data1 = vec![1u8; 100];
        buffer.update(key1, &data1).unwrap();

        let offset1 = buffer.offset(key1).unwrap();
        let size1 = buffer.size(key1).unwrap();

        assert_eq!(offset1, 0); // First allocation
        assert!(size1 >= 100);
        assert!(size1.is_power_of_two());

        // Add another
        let data2 = vec![2u8; 300];
        buffer.update(key2, &data2).unwrap();

        let offset2 = buffer.offset(key2).unwrap();
        let size2 = buffer.size(key2).unwrap();

        assert_ne!(offset1, offset2);
        assert!(size2 >= 300);
        assert!(size2.is_power_of_two());

        // Remove and check
        buffer.remove(key1);
        assert_eq!(buffer.offset(key1), None);
        assert_eq!(buffer.size(key1), None);
    }

    #[test]
    fn test_update_smaller_data_clears_tail() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Insert larger data
        let large_data = vec![0xAA; 200];
        buffer.update(key1, &large_data).unwrap();

        let offset = buffer.offset(key1).unwrap();
        let size = buffer.size(key1).unwrap();

        // Update with smaller data
        let small_data = vec![0xBB; 50];
        buffer.update(key1, &small_data).unwrap();

        // Should reuse same allocation
        assert_eq!(buffer.offset(key1), Some(offset));
        assert_eq!(buffer.size(key1), Some(size));

        // First 50 bytes should be new data
        assert_eq!(&buffer.raw_data[offset..offset + 50], &small_data[..]);

        // Rest should be cleared to zero
        for i in 50..size {
            assert_eq!(
                buffer.raw_data[offset + i],
                0,
                "Byte at offset {} not cleared",
                i
            );
        }
    }

    #[test]
    fn test_helper_functions() {
        // Test round_pow2
        assert_eq!(round_pow2(0), MIN_BLOCK);
        assert_eq!(round_pow2(1), MIN_BLOCK);
        assert_eq!(round_pow2(MIN_BLOCK), MIN_BLOCK);
        assert_eq!(round_pow2(MIN_BLOCK + 1), MIN_BLOCK * 2);
        assert_eq!(round_pow2(1000), 1024);
        assert_eq!(round_pow2(1024), 1024);
        assert_eq!(round_pow2(1025), 2048);

        // Test index/offset conversions
        let leaves = 4; // 1024 bytes / 256 MIN_BLOCK = 4 leaves

        // Leaf indices are 3, 4, 5, 6 in a tree with 4 leaves
        assert_eq!(offset_to_index(0, leaves), 3);
        assert_eq!(offset_to_index(MIN_BLOCK, leaves), 4);
        assert_eq!(offset_to_index(MIN_BLOCK * 2, leaves), 5);
        assert_eq!(offset_to_index(MIN_BLOCK * 3, leaves), 6);

        // Test reverse conversion
        assert_eq!(index_to_offset(3, leaves), 0);
        assert_eq!(index_to_offset(4, leaves), MIN_BLOCK);
        assert_eq!(index_to_offset(5, leaves), MIN_BLOCK * 2);
        assert_eq!(index_to_offset(6, leaves), MIN_BLOCK * 3);

        // Test internal nodes (should walk to leftmost leaf)
        assert_eq!(index_to_offset(0, leaves), 0); // Root -> leftmost leaf
        assert_eq!(index_to_offset(1, leaves), 0); // Left child -> leftmost leaf
        assert_eq!(index_to_offset(2, leaves), MIN_BLOCK * 2); // Right child -> its leftmost
    }

    #[test]
    fn test_complex_allocation_deallocation_pattern() {
        let mut buffer: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(4096, Some("complex_pattern_test".to_string()));

        let mut key_map = SlotMap::new();
        let mut allocations = Vec::new();

        // Create a complex pattern of allocations
        for i in 0..10 {
            let key = key_map.insert(());
            let size = MIN_BLOCK * (1 << (i % 3)); // Sizes: 256, 512, 1024, 256, ...
            let data = vec![(i % 256) as u8; size];

            buffer.update(key, &data).unwrap();
            allocations.push((key, size));
        }

        // Remove some allocations in a pattern
        for i in (1..10).step_by(3) {
            buffer.remove(allocations[i].0);
        }

        // Add new allocations that might fit in gaps
        for i in 20..25 {
            let key = key_map.insert(());
            let size = MIN_BLOCK * (1 << (i % 2)); // Sizes: 256, 512, 256, ...
            let data = vec![(i % 256) as u8; size];

            let offset = buffer.update(key, &data).unwrap();

            // Verify data was written correctly
            for j in 0..size {
                assert_eq!(
                    buffer.raw_data[offset + j],
                    (i % 256) as u8,
                    "Data corruption at offset {}",
                    offset + j
                );
            }
        }
    }

    #[test]
    fn test_extreme_fragmentation_handling() {
        let mut buffer: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(8192, Some("fragmentation_test".to_string()));

        let mut key_map = SlotMap::new();
        let mut keys = Vec::new();

        // Create maximum fragmentation: allocate all MIN_BLOCK sized chunks
        let num_blocks = 8192 / MIN_BLOCK;
        for i in 0..num_blocks {
            let key = key_map.insert(());
            keys.push(key);
            buffer.update(key, &vec![i as u8; MIN_BLOCK]).unwrap();
        }

        // Remove every other block
        for i in (0..num_blocks).step_by(2) {
            buffer.remove(keys[i]);
        }

        // Try to allocate a larger block - should trigger growth
        let large_key = key_map.insert(());
        let large_data = vec![0xFF; MIN_BLOCK * 4];

        let offset = buffer.update(large_key, &large_data).unwrap();

        // Should have grown the buffer
        assert!(buffer.raw_data.len() > 8192);

        // Data should be intact
        assert_eq!(
            &buffer.raw_data[offset..offset + large_data.len()],
            &large_data[..]
        );
    }

    #[test]
    fn test_new_utility_methods() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, _) = create_keys();

        // Test is_empty and len
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);

        buffer.update(key1, b"data1").unwrap();
        assert!(!buffer.is_empty());
        assert_eq!(buffer.len(), 1);

        buffer.update(key2, b"data2_longer").unwrap();
        assert_eq!(buffer.len(), 2);

        // Test contains_key
        assert!(buffer.contains_key(key1));
        assert!(buffer.contains_key(key2));

        // Test capacity
        assert_eq!(buffer.capacity(), 1024);

        // Test keys iterator
        let keys: Vec<_> = buffer.keys().collect();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&key1));
        assert!(keys.contains(&key2));

        buffer.remove(key1);
        assert_eq!(buffer.len(), 1);
        assert!(!buffer.contains_key(key1));
        assert!(buffer.contains_key(key2));
    }

    #[test]
    fn test_zero_sized_allocation() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Zero-sized allocation should still work and allocate MIN_BLOCK
        buffer.update(key1, &[]).unwrap();

        assert!(buffer.contains_key(key1));
        assert_eq!(buffer.size(key1), Some(MIN_BLOCK));
        assert_eq!(buffer.offset(key1), Some(0));
    }

    #[test]
    fn test_maximum_fragmentation_recovery() {
        let mut buffer: DynamicStorageBuffer<TestKey> = DynamicStorageBuffer::new(2048, None);
        let mut key_map = SlotMap::new();
        let mut keys = Vec::new();

        // Create maximum fragmentation
        for i in 0..8 {
            let key = key_map.insert(());
            keys.push(key);
            buffer.update(key, &vec![i as u8; MIN_BLOCK]).unwrap();
        }

        // Remove alternating allocations
        for i in (0..8).step_by(2) {
            buffer.remove(keys[i]);
        }

        // Now we have fragmented memory - try to allocate something that fits
        let key_new = key_map.insert(());
        buffer.update(key_new, &vec![0xFF; MIN_BLOCK]).unwrap();

        // Should reuse one of the freed blocks
        let offset = buffer.offset(key_new).unwrap();
        assert!(offset % (MIN_BLOCK * 2) == 0, "Should reuse a freed block");
    }

    #[test]
    fn test_concurrent_like_access_pattern() {
        let mut buffer = create_test_buffer();
        let mut key_map = SlotMap::new();
        let mut operations = Vec::new();

        // Simulate mixed operations
        for i in 0..20 {
            let key = key_map.insert(());
            let size = 50 + (i * 17) % 200; // Varying sizes
            let data = vec![(i % 256) as u8; size];

            buffer.update(key, &data).unwrap();
            operations.push((key, data));

            // Sometimes remove older items
            if i > 5 && i % 3 == 0 {
                let idx = (i - 5) / 2;
                if idx < operations.len() {
                    buffer.remove(operations[idx].0);
                }
            }
        }

        // Verify remaining data integrity
        for (key, expected_data) in &operations {
            if let Some(offset) = buffer.offset(*key) {
                let actual = &buffer.raw_data[offset..offset + expected_data.len()];
                assert_eq!(actual, expected_data.as_slice());
            }
        }
    }

    #[test]
    fn test_growth_with_multiple_size_requirements() {
        let mut buffer: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(512, Some("multi_growth_test".to_string()));

        let (mut key_map, _, _, _) = create_keys();

        // Test that growth accommodates the required size
        let key1 = key_map.insert(());
        let huge_data = vec![0x42; 2048];

        buffer.update(key1, &huge_data).unwrap();

        // Buffer should have grown enough to accommodate the data
        assert!(buffer.raw_data.len() >= 2048);

        // Data should be stored correctly
        let offset = buffer.offset(key1).unwrap();
        assert_eq!(
            &buffer.raw_data[offset..offset + huge_data.len()],
            &huge_data[..]
        );
    }

    #[test]
    fn test_capacity_overflow_returns_error() {
        let mut buffer: DynamicStorageBuffer<TestKey> =
            DynamicStorageBuffer::new(MIN_BLOCK, Some("overflow_test".to_string()));

        // Exercise the real overflow path deterministically: asking to grow by
        // usize::MAX makes `old_cap + min_extra` and the subsequent doublings
        // overflow, which `grow` must surface as `CapacityOverflow` rather than
        // panicking.
        let result = buffer.grow(usize::MAX);
        assert_eq!(result, Err(DynamicStorageBufferError::CapacityOverflow));

        // Verify the error message remains meaningful.
        assert!(format!("{}", DynamicStorageBufferError::CapacityOverflow)
            .contains("capacity overflow"));
    }
}
