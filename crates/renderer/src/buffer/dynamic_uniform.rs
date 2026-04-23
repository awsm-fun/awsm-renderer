//! Dynamic uniform buffer utilities.

use slotmap::{Key, SecondaryMap};

/// Dynamic buffer for fixed-size allocations with efficient slot reuse.
///
/// This buffer is optimized for managing many items of identical size,
/// automatically handling slot allocation, reuse, and buffer growth.
/// All items must be the same size (specified at creation time).
///
///-------------------------------- PERFORMANCE SUMMARY ------------------------//
///
/// • insert/update/remove:   O(1)  (amortized, ignoring rare growth)
/// • GPU write (per frame):  uploads dirty ranges (full upload when dense)
/// • Resize strategy:        doubles capacity when needed (infrequent pauses)
/// • External fragmentation: none (fixed-size slots)
/// • Internal fragmentation: none
/// • Memory overhead:        exactly `capacity × ALIGNED_SLICE_SIZE`
///
/// • Ideal usage:
///    Thousands of items with identical sizes, like:
///      - Transforms
///      - Lights
///      - PBR Materials
///
///----------------------------------------------------------------------------//
///
/// This gives us a generic helper for dynamic buffers of a fixed alignment size
/// It internally manages free slots for re‑use, and reallocates (grows) the underlying buffer only when needed.
///
/// The bind group layout and bind group are created once (and updated on buffer reallocation)
/// so that even with thousands of draw calls, we only use one bind group layout.
///
/// This is particularly useful for things like transforms and morph weights which have a fixed size,
/// but may be inserted/removed at any time, so we can re-use their slots
/// without having to reallocate the entire buffer every time.
///
/// This also has the benefit of not needing complicated logic to avoid coalescing etc.
#[derive(Debug, Clone)]
pub struct DynamicUniformBuffer<K: Key, const ZERO_VALUE: u8 = 0> {
    /// Raw CPU‑side data for all items, organized in BYTE_SIZE slots.
    raw_data: Vec<u8>,
    dirty_ranges: Vec<(usize, usize)>,
    /// The GPU buffer storing the raw data.
    gpu_buffer_needs_resize: bool,
    /// Mapping from a Key to a slot index within the buffer.
    slot_indices: SecondaryMap<K, usize>,
    /// List of free slot indices available for reuse.
    free_slots: Vec<usize>,
    /// Total capacity of the buffer in number of slots.
    capacity_slots: usize,
    // first unused index >= capacity used so far
    next_slot: usize,
    label: Option<String>,
    byte_size: usize,
    aligned_slice_size: usize,
    /// The original `initial_capacity` argument passed to [`Self::new`],
    /// kept so that [`Self::clear`] can reset to the same starting size.
    initial_capacity_arg: usize,
}

impl<K: Key, const ZERO_VALUE: u8> DynamicUniformBuffer<K, ZERO_VALUE> {
    #[allow(clippy::too_many_arguments)]
    /// Creates a new dynamic uniform buffer.
    pub fn new(
        initial_capacity: usize,
        byte_size: usize,
        aligned_slice_size: Option<usize>,
        label: Option<String>,
    ) -> Self {
        let aligned_slice_size = aligned_slice_size.unwrap_or(byte_size);
        let initial_size_bytes: usize = initial_capacity * aligned_slice_size;
        // Allocate CPU data – initially filled with zeros.
        let raw_data = vec![ZERO_VALUE; initial_size_bytes];

        Self {
            raw_data,
            dirty_ranges: Vec::new(),
            gpu_buffer_needs_resize: false,
            slot_indices: SecondaryMap::new(),
            free_slots: (0..initial_capacity).rev().collect(), // Reverse so slot 0 is used first
            capacity_slots: initial_capacity,
            next_slot: initial_capacity,
            label,
            byte_size,
            aligned_slice_size,
            initial_capacity_arg: initial_capacity,
        }
    }

    /// Resets the buffer to its initial capacity, dropping all allocations.
    ///
    /// After this call the buffer is in the same state as a freshly
    /// constructed one (with the same constructor arguments).
    pub fn clear(&mut self) {
        let fresh = Self::new(
            self.initial_capacity_arg,
            self.byte_size,
            Some(self.aligned_slice_size),
            self.label.clone(),
        );
        *self = fresh;
    }

    /// Returns the total byte size of the buffer.
    pub fn size(&self) -> usize {
        self.raw_data.len()
    }

    /// Updates an item in the buffer using a callback function.
    ///
    /// The callback receives:
    /// - The byte offset of the slot in the buffer
    /// - A mutable slice of exactly `byte_size` bytes to write into
    ///
    /// This method efficiently:
    /// - Reuses existing slot if the key already exists
    /// - Allocates from free slots when available
    /// - Grows the buffer automatically when needed
    ///
    /// Note: GPU buffer is not updated until `take_gpu_needs_resize()` is called.
    pub fn update_with(&mut self, key: K, f: impl FnOnce(usize, &mut [u8])) {
        // If we don't have a slot, set one
        let slot = match self.slot_indices.get(key) {
            Some(slot) => *slot,
            None => {
                // Choose a slot: either reuse a free slot or use the next available slot.
                let slot = if let Some(free_slot) = self.free_slots.pop() {
                    free_slot
                } else {
                    let new_slot = self.next_slot;
                    // Check if we need to grow the raw_data and GPU buffer.
                    if (new_slot + 1) * self.aligned_slice_size > self.raw_data.len() {
                        self.resize(new_slot + 1);
                    }
                    self.next_slot += 1;
                    new_slot
                };

                self.slot_indices.insert(key, slot);

                slot
            }
        };

        // Calculate byte offset.
        let offset_bytes = slot * self.aligned_slice_size;

        // we can mutate the slice directly
        f(
            offset_bytes,
            &mut self.raw_data[offset_bytes..offset_bytes + self.byte_size],
        );
        self.mark_dirty_range(offset_bytes, self.byte_size);
    }

    /// Updates an item in the buffer with raw bytes.
    ///
    /// # Panics
    /// Panics if `values.len()` exceeds `byte_size`.
    pub fn update(&mut self, key: K, values: &[u8]) {
        self.update_with(key, |_, data| {
            data[..values.len()].copy_from_slice(values);
        })
    }

    /// Gets an immutable view into the slice
    pub fn get(&self, key: K) -> Option<&[u8]> {
        let slot = self.slot_indices.get(key)?;
        let offset_bytes = slot * self.aligned_slice_size;
        Some(&self.raw_data[offset_bytes..offset_bytes + self.byte_size])
    }

    /// Updates a portion of an existing slot starting at the given offset.
    ///
    /// # Panics
    /// Panics if `offset + values.len()` exceeds `byte_size`.
    pub fn update_offset(&mut self, key: K, offset: usize, values: &[u8]) {
        self.update_with(key, |_, data| {
            data[offset..offset + values.len()].copy_from_slice(values);
        })
    }

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

    /// Removes the slot corresponding to the given key.
    /// The slot is marked as free for reuse.
    /// returns whether or not it was actually removed
    pub fn remove(&mut self, key: K) -> bool {
        if let Some(slot) = self.slot_indices.remove(key) {
            // Add this slot to the free list.
            self.free_slots.push(slot);

            // Zero out the data in the slot.
            let offset_bytes = slot * self.aligned_slice_size;
            self.raw_data[offset_bytes..offset_bytes + self.aligned_slice_size].fill(ZERO_VALUE);
            self.mark_dirty_range(offset_bytes, self.aligned_slice_size);
            true
        } else {
            false
        }
    }

    /// Returns the byte offset for a key.
    pub fn offset(&self, key: K) -> Option<usize> {
        let slot = self.slot_indices.get(key)?;

        Some(slot * self.aligned_slice_size)
    }

    /// Returns the slot index for a key.
    pub fn slot_index(&self, key: K) -> Option<usize> {
        self.slot_indices.get(key).copied()
    }

    /// Returns an iterator over keys.
    pub fn keys<'a>(&'a self) -> slotmap::secondary::Keys<'a, K, usize> {
        self.slot_indices.keys()
    }

    /// Returns the number of currently allocated keys
    pub fn len(&self) -> usize {
        self.slot_indices.len()
    }

    /// Returns true if no keys are allocated
    pub fn is_empty(&self) -> bool {
        self.slot_indices.is_empty()
    }

    /// Returns the current capacity in number of slots
    pub fn capacity(&self) -> usize {
        self.capacity_slots
    }

    /// Returns the number of free slots available for reuse
    pub fn free_slots_count(&self) -> usize {
        self.free_slots.len()
    }

    /// Checks if a key exists in the buffer
    pub fn contains_key(&self, key: K) -> bool {
        self.slot_indices.contains_key(key)
    }

    fn resize(&mut self, required_slots: usize) {
        // grow to at least double, exactly like Vec
        let new_cap = required_slots.max(self.capacity_slots) * 2;

        self.raw_data
            .resize(new_cap * self.aligned_slice_size, ZERO_VALUE);

        // Add new slots to free_slots, starting from required_slots to avoid
        // duplicating the slot that's about to be allocated (required_slots - 1)
        self.free_slots.extend(required_slots..new_cap);

        // Update next_slot to point beyond all slots that could be reused from free_slots
        // This prevents duplicate slot assignments when free_slots is exhausted
        self.next_slot = new_cap;

        self.capacity_slots = new_cap;
        self.gpu_buffer_needs_resize = true;
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
}

#[cfg(test)]
mod test {
    use super::*;
    use slotmap::SlotMap;

    type TestKey = slotmap::DefaultKey;

    fn create_test_buffer() -> DynamicUniformBuffer<TestKey> {
        DynamicUniformBuffer::new(
            2,        // initial capacity of 2 slots
            16,       // 16 bytes per item
            Some(32), // 32 bytes aligned size (with padding)
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

        // Initial capacity should be 2 slots * 32 bytes = 64 bytes
        assert_eq!(buffer.size(), 64);
        assert_eq!(buffer.capacity_slots, 2);
        assert_eq!(buffer.next_slot, 2);
        assert_eq!(buffer.free_slots.len(), 2);
        assert_eq!(buffer.byte_size, 16);
        assert_eq!(buffer.aligned_slice_size, 32);

        // All slots should initially be free (reversed so slot 0 is used first)
        assert_eq!(buffer.free_slots, vec![1, 0]);

        // No keys should be assigned
        assert_eq!(buffer.slot_indices.len(), 0);
    }

    #[test]
    fn test_insert_single_item() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        let test_data = b"hello world 1234"; // 16 bytes
        buffer.update(key1, test_data);

        // Should use the first free slot (index 0, since free_slots is [1, 0] and we pop from end)
        assert_eq!(buffer.slot_indices.get(key1), Some(&0));
        assert_eq!(buffer.free_slots, vec![1]);

        // Check that data was written correctly
        let offset = buffer.offset(key1).unwrap();
        assert_eq!(offset, 0); // slot 0 * 32 bytes
        assert_eq!(&buffer.raw_data[offset..offset + 16], test_data);
    }

    #[test]
    fn test_insert_multiple_items() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, _) = create_keys();

        let data1 = b"data for key one";
        let data2 = b"data for key two";

        buffer.update(key1, data1);
        buffer.update(key2, data2);

        // Both free slots should be used
        assert!(buffer.slot_indices.contains_key(key1));
        assert!(buffer.slot_indices.contains_key(key2));
        assert_eq!(buffer.free_slots.len(), 0);

        // Verify data integrity
        let offset1 = buffer.offset(key1).unwrap();
        let offset2 = buffer.offset(key2).unwrap();
        assert_ne!(offset1, offset2);
        assert_eq!(&buffer.raw_data[offset1..offset1 + 16], data1);
        assert_eq!(&buffer.raw_data[offset2..offset2 + 16], data2);
    }

    #[test]
    fn test_buffer_growth() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, key3) = create_keys();

        // Fill initial capacity
        buffer.update(key1, b"data one 1234567");
        buffer.update(key2, b"data two 1234567");

        // This should trigger growth
        let initial_size = buffer.size();
        buffer.update(key3, b"data three 12345");

        // Buffer should have doubled in capacity (2 -> 4 slots, but we needed 3, so it grows to accommodate the required slots)
        // The resize logic actually grows to max(required_slots, current_capacity) * 2
        // Since we had 2 slots, needed 3, it goes to max(3, 2) * 2 = 6 slots
        assert_eq!(buffer.capacity_slots, 6);
        assert_eq!(buffer.size(), 192); // 6 slots * 32 bytes
        assert!(buffer.size() > initial_size);

        // All data should still be accessible
        assert!(buffer.offset(key1).is_some());
        assert!(buffer.offset(key2).is_some());
        assert!(buffer.offset(key3).is_some());
    }

    #[test]
    fn test_gpu_resize_flag() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, key3) = create_keys();

        // Initially no resize needed
        assert_eq!(buffer.take_gpu_needs_resize(), None);

        // Fill capacity without triggering growth
        buffer.update(key1, b"test data 123456");
        assert_eq!(buffer.take_gpu_needs_resize(), None);

        // Trigger growth
        buffer.update(key2, b"more test data12");
        buffer.update(key3, b"even more data12");

        // Should indicate resize needed - size will be 6 slots * 32 bytes = 192
        assert_eq!(buffer.take_gpu_needs_resize(), Some(192));

        // Flag should be reset after taking
        assert_eq!(buffer.take_gpu_needs_resize(), None);
    }

    #[test]
    fn test_update_existing_item() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Insert initial data
        buffer.update(key1, b"initial data1234");
        let initial_offset = buffer.offset(key1).unwrap();

        // Update with new data
        buffer.update(key1, b"updated data1234");
        let updated_offset = buffer.offset(key1).unwrap();

        // Should use same slot
        assert_eq!(initial_offset, updated_offset);

        // Data should be updated
        assert_eq!(
            &buffer.raw_data[updated_offset..updated_offset + 16],
            b"updated data1234"
        );
    }

    #[test]
    fn test_update_with_callback() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        buffer.update_with(key1, |offset, data| {
            assert_eq!(data.len(), 16);
            data[0..4].copy_from_slice(b"test");
            data[4..8].copy_from_slice(&(offset as u32).to_le_bytes());
        });

        let offset = buffer.offset(key1).unwrap();
        assert_eq!(&buffer.raw_data[offset..offset + 4], b"test");
        assert_eq!(
            &buffer.raw_data[offset + 4..offset + 8],
            &(offset as u32).to_le_bytes()
        );
    }

    #[test]
    fn test_update_offset() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Initialize with zeros
        buffer.update(key1, &[0u8; 16]);

        // Update specific offset
        buffer.update_offset(key1, 8, b"partial");

        let offset = buffer.offset(key1).unwrap();
        let data = &buffer.raw_data[offset..offset + 16];

        // First 8 bytes should be zero
        assert_eq!(&data[0..8], &[0u8; 8]);
        // Next 7 bytes should be "partial"
        assert_eq!(&data[8..15], b"partial");
        // Last byte should be zero
        assert_eq!(data[15], 0);
    }

    #[test]
    fn test_remove_item() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, _) = create_keys();

        // Insert two items
        buffer.update(key1, b"data one 1234567");
        buffer.update(key2, b"data two 1234567");

        let offset1 = buffer.offset(key1).unwrap();
        assert_eq!(buffer.free_slots.len(), 0);

        // Remove first item
        buffer.remove(key1);

        // Key should no longer exist
        assert_eq!(buffer.offset(key1), None);
        assert!(!buffer.slot_indices.contains_key(key1));

        // Slot should be available for reuse
        assert_eq!(buffer.free_slots.len(), 1);

        // Data should be zeroed out
        assert_eq!(&buffer.raw_data[offset1..offset1 + 32], &[0u8; 32]);

        // Second key should still work
        assert!(buffer.offset(key2).is_some());
    }

    #[test]
    fn test_slot_reuse_after_removal() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, key3) = create_keys();

        // Fill buffer
        buffer.update(key1, b"first item 12345");
        buffer.update(key2, b"second item 1234");

        let offset1 = buffer.offset(key1).unwrap();

        // Remove first item
        buffer.remove(key1);

        // Insert new item - should reuse the freed slot
        buffer.update(key3, b"third item 12345");
        let offset3 = buffer.offset(key3).unwrap();

        // Should reuse the same slot
        assert_eq!(offset1, offset3);
        assert_eq!(buffer.free_slots.len(), 0);

        // Data should be the new data
        assert_eq!(&buffer.raw_data[offset3..offset3 + 16], b"third item 12345");
    }

    #[test]
    fn test_remove_nonexistent_key() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Remove key that was never inserted
        buffer.remove(key1);

        // Should not crash or change state
        assert_eq!(buffer.slot_indices.len(), 0);
        assert_eq!(buffer.free_slots.len(), 2);
    }

    #[test]
    fn test_keys_iterator() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, _key3) = create_keys();

        // Initially no keys
        assert_eq!(buffer.keys().count(), 0);

        // Add some items
        buffer.update(key1, b"data1234567890ab");
        buffer.update(key2, b"more data 123456");

        let keys: Vec<_> = buffer.keys().collect();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&key1));
        assert!(keys.contains(&key2));

        // Remove one
        buffer.remove(key1);
        let keys: Vec<_> = buffer.keys().collect();
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&key2));
        assert!(!keys.contains(&key1));
    }

    #[test]
    fn test_zero_value_variants() {
        // Test with default zero value (0)
        let mut buffer1: DynamicUniformBuffer<TestKey, 0> =
            DynamicUniformBuffer::new(1, 8, Some(16), Some("zero_buffer".to_string()));

        // Test with custom zero value (0xFF)
        let mut buffer2: DynamicUniformBuffer<TestKey, 0xFF> =
            DynamicUniformBuffer::new(1, 8, Some(16), Some("ones_buffer".to_string()));

        let (_, key1, key2, _) = create_keys();

        // Add and remove items to test zero fill behavior
        buffer1.update(key1, b"testdata");
        buffer2.update(key2, b"testdata");

        buffer1.remove(key1);
        buffer2.remove(key2);

        // Check that removed slots are filled with the correct zero value
        assert_eq!(&buffer1.raw_data[0..16], &[0u8; 16]);
        assert_eq!(&buffer2.raw_data[0..16], &[0xFFu8; 16]);
    }

    #[test]
    fn test_large_scale_operations() {
        let mut buffer: DynamicUniformBuffer<TestKey> = DynamicUniformBuffer::new(
            8, // Start small
            16,
            Some(32),
            Some("stress_test".to_string()),
        );

        let mut key_map = SlotMap::new();
        let mut keys = Vec::new();

        // Insert many items
        for i in 0..100 {
            let key = key_map.insert(());
            keys.push(key);

            let data = format!("item_{:03}_{:08}", i, i * 12345);
            let mut bytes = data.as_bytes().to_vec();
            bytes.resize(16, 0);

            buffer.update(key, &bytes);
        }

        // Verify all items are accessible
        assert_eq!(buffer.keys().count(), 100);

        // Remove every other item
        for (i, &key) in keys.iter().enumerate() {
            if i % 2 == 0 {
                buffer.remove(key);
            }
        }

        assert_eq!(buffer.keys().count(), 50);

        // Add new items that should reuse freed slots
        for i in 200..250 {
            let key = key_map.insert(());
            let data = format!("new_item_{:03}", i);
            let mut bytes = data.as_bytes().to_vec();
            bytes.resize(16, 0);

            buffer.update(key, &bytes);
        }

        assert_eq!(buffer.keys().count(), 100);
    }

    #[test]
    fn test_raw_slice_access() {
        let mut buffer = create_test_buffer();
        let (_, key1, _, _) = create_keys();

        // Initially should be all zeros
        let raw = buffer.raw_slice();
        assert_eq!(raw.len(), 64); // 2 slots * 32 bytes
        assert_eq!(raw, &[0u8; 64]);

        // Add data
        buffer.update(key1, b"test data conten"); // Exactly 16 bytes

        // Raw slice should reflect the changes
        let raw = buffer.raw_slice();
        let offset = buffer.offset(key1).unwrap();
        assert_eq!(&raw[offset..offset + 16], b"test data conten");
    }

    #[test]
    fn test_alignment_behavior() {
        // Test that byte_size and aligned_slice_size work correctly
        let mut buffer: DynamicUniformBuffer<TestKey> = DynamicUniformBuffer::new(
            2,
            10,       // Only 10 bytes of actual data
            Some(16), // But aligned to 16 bytes
            None,
        );

        let (_, key1, key2, _) = create_keys();

        buffer.update(key1, &[1u8; 10]);
        buffer.update(key2, &[2u8; 10]);

        let offset1 = buffer.offset(key1).unwrap();
        let offset2 = buffer.offset(key2).unwrap();

        // Offsets should be aligned_slice_size apart
        assert_eq!((offset2 as i32 - offset1 as i32).abs(), 16);

        // But only 10 bytes of actual data should be set
        assert_eq!(&buffer.raw_data[offset1..offset1 + 10], &[1u8; 10]);
        assert_eq!(&buffer.raw_data[offset2..offset2 + 10], &[2u8; 10]);
    }

    #[test]
    fn test_resize_slot_allocation_correctness() {
        // This test verifies that when resize occurs, allocated slots are not added to free_slots
        let mut buffer = create_test_buffer(); // 2 slots initially
        let mut key_map = SlotMap::new();

        // Fill the initial capacity (2 slots)
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());
        buffer.update(key1, b"data1_1234567890");
        buffer.update(key2, b"data2_1234567890");

        // Verify initial state
        assert_eq!(buffer.free_slots.len(), 0);
        assert_eq!(buffer.next_slot, 2);
        assert_eq!(buffer.capacity_slots, 2);

        // Add a third item, which triggers resize
        let key3 = key_map.insert(());
        buffer.update(key3, b"data3_1234567890");

        // After resize, capacity should have grown
        assert_eq!(buffer.capacity_slots, 6); // max(3, 2) * 2 = 6

        // Verify key3 was assigned slot 2 (next_slot was 2 before resize)
        assert_eq!(buffer.slot_indices.get(key3), Some(&2));

        // Verify slot 2 is not in free_slots since it's allocated
        assert!(
            !buffer.free_slots.contains(&2),
            "Allocated slot 2 should not appear in free_slots"
        );

        // Free slots should be [3, 4, 5] after the resize
        let mut expected_free_slots = vec![3, 4, 5];
        let mut actual_free_slots = buffer.free_slots.clone();
        expected_free_slots.sort();
        actual_free_slots.sort();
        assert_eq!(actual_free_slots, expected_free_slots);

        // Allocate one more key to verify correct slot assignment
        let key4 = key_map.insert(());
        buffer.update(key4, b"data4_1234567890");

        // key4 should get a free slot (one of 3, 4, or 5), not the allocated slot 2
        let slot4 = *buffer.slot_indices.get(key4).unwrap();
        assert_ne!(slot4, 2, "New key should not reuse allocated slot 2");
        assert!(
            (3..=5).contains(&slot4),
            "New key should use a slot from free_slots"
        );

        // Verify all data integrity
        let offset3 = buffer.offset(key3).unwrap();
        assert_eq!(&buffer.raw_data[offset3..offset3 + 16], b"data3_1234567890");

        let offset4 = buffer.offset(key4).unwrap();
        assert_eq!(&buffer.raw_data[offset4..offset4 + 16], b"data4_1234567890");
    }

    #[test]
    fn test_resize_with_required_slots_exceeding_capacity() {
        // Test resize behavior when required_slots > capacity_slots
        let mut buffer: DynamicUniformBuffer<TestKey> = DynamicUniformBuffer::new(
            2,        // initial capacity of 2 slots
            16,       // 16 bytes per item
            Some(32), // 32 bytes aligned size
            Some("capacity_test".to_string()),
        );

        let mut key_map = SlotMap::new();

        // Simulate a state where next_slot exceeds capacity
        buffer.free_slots.clear();
        buffer.next_slot = 5;

        println!(
            "Before update: capacity_slots={}, next_slot={}, free_slots={:?}",
            buffer.capacity_slots, buffer.next_slot, buffer.free_slots
        );

        // Allocate a key when next_slot (5) > capacity (2)
        let key1 = key_map.insert(());
        buffer.update(key1, b"capacity_test_12");

        println!(
            "After update: capacity_slots={}, next_slot={}, free_slots={:?}",
            buffer.capacity_slots, buffer.next_slot, buffer.free_slots
        );

        // The resize should trigger with required_slots = 6 (next_slot + 1)
        // new_cap = max(6, 2) * 2 = 12
        assert_eq!(buffer.capacity_slots, 12);

        // Verify free_slots contains slots from 6 to 11
        let mut free_slots_sorted = buffer.free_slots.clone();
        free_slots_sorted.sort();

        let expected_free: Vec<usize> = (6..12).collect();
        assert_eq!(
            free_slots_sorted, expected_free,
            "Free slots after resize are incorrect: {:?}",
            free_slots_sorted
        );

        // Slot 5 should be assigned to key1, not in free_slots
        assert_eq!(buffer.slot_indices.get(key1), Some(&5));
        assert!(!buffer.free_slots.contains(&5));
    }

    #[test]
    fn test_resize_slot_range_allocation() {
        // Test that the resize slot range calculation is correct
        let mut buffer = create_test_buffer();
        let mut key_map = SlotMap::new();

        // Fill initial slots
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());
        buffer.update(key1, b"data1_1234567890");
        buffer.update(key2, b"data2_1234567890");

        // Current state: capacity=2, next_slot=2, free_slots=[]

        // Add a third item to trigger resize
        let key3 = key_map.insert(());

        // The resize logic:
        // required_slots = next_slot + 1 = 3
        // new_cap = max(3, 2) * 2 = 6
        // extend(3..6) adds [3, 4, 5]
        // slot 2 should be allocated to key3

        buffer.update(key3, b"data3_1234567890");

        println!(
            "After resize: capacity={}, next_slot={}, free_slots={:?}",
            buffer.capacity_slots, buffer.next_slot, buffer.free_slots
        );

        // Verify key3 got slot 2
        assert_eq!(buffer.slot_indices.get(key3), Some(&2));

        // Verify free_slots are [3, 4, 5] and slot 2 is not included
        let mut sorted_free = buffer.free_slots.clone();
        sorted_free.sort();
        assert_eq!(sorted_free, vec![3, 4, 5]);
        assert!(!buffer.free_slots.contains(&2));

        // Allocate more keys to verify no conflicts
        let key4 = key_map.insert(());
        buffer.update(key4, b"data4_1234567890");

        let slot4 = *buffer.slot_indices.get(key4).unwrap();
        assert_eq!(slot4, 5); // Should pop from end of free_slots

        // Verify no slot conflicts exist
        for (key, &slot) in buffer.slot_indices.iter() {
            assert!(
                !buffer.free_slots.contains(&slot),
                "Slot {} is both allocated to key {:?} and in free_slots",
                slot,
                key
            );
        }
    }

    #[test]
    fn test_resize_slot_assignment_consistency() {
        // Test that resize correctly handles slot assignment edge cases
        let mut buffer: DynamicUniformBuffer<TestKey> =
            DynamicUniformBuffer::new(2, 16, Some(32), Some("consistency_test".to_string()));

        let mut key_map = SlotMap::new();

        // Create a state where next_slot exceeds capacity to test edge case handling
        buffer.free_slots.clear();
        buffer.next_slot = 3;

        println!(
            "Forced state: capacity={}, next_slot={}, free_slots={:?}",
            buffer.capacity_slots, buffer.next_slot, buffer.free_slots
        );

        // Allocation should:
        // 1. Use next_slot = 3 as new_slot
        // 2. Trigger resize(3 + 1) where required_slots = 4
        // 3. new_cap = max(4, 2) * 2 = 8
        // 4. extend(4..8) adds [4, 5, 6, 7]
        // 5. Slot 3 should be allocated

        let key1 = key_map.insert(());
        buffer.update(key1, b"test_slot_3_data");

        println!(
            "After update: capacity={}, next_slot={}, free_slots={:?}, slot_for_key1={:?}",
            buffer.capacity_slots,
            buffer.next_slot,
            buffer.free_slots,
            buffer.slot_indices.get(key1)
        );

        // Verify key1 got slot 3
        assert_eq!(buffer.slot_indices.get(key1), Some(&3));

        // Verify slot 3 is not in free_slots
        assert!(
            !buffer.free_slots.contains(&3),
            "Allocated slot should not be in free_slots"
        );

        // free_slots should be [4, 5, 6, 7]
        let mut sorted_free = buffer.free_slots.clone();
        sorted_free.sort();
        assert_eq!(sorted_free, vec![4, 5, 6, 7]);

        // Test allocating another item to verify no conflicts
        let key2 = key_map.insert(());
        buffer.update(key2, b"test_slot_7_data");

        let slot2 = *buffer.slot_indices.get(key2).unwrap();
        assert_eq!(slot2, 7); // Should pop from end
        assert_ne!(slot2, 3, "No slot conflicts should occur");
    }

    #[test]
    fn test_resize_semantic_correctness() {
        // Test that resize correctly handles the relationship between required_slots and capacity
        let mut buffer = create_test_buffer();
        let mut key_map = SlotMap::new();

        // Fill initial slots
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());
        buffer.update(key1, b"data1_1234567890");
        buffer.update(key2, b"data2_1234567890");

        // Current state: capacity=2, next_slot=2, free_slots=[]

        // Remove one to create a free slot
        buffer.remove(key1);
        let freed_slot = if buffer.slot_indices.get(key2) == Some(&0) {
            1
        } else {
            0
        };
        assert!(buffer.free_slots.contains(&freed_slot));

        println!(
            "Before third allocation: capacity={}, next_slot={}, free_slots={:?}",
            buffer.capacity_slots, buffer.next_slot, buffer.free_slots
        );

        // Add third item - should reuse freed slot, not trigger resize
        let key3 = key_map.insert(());
        buffer.update(key3, b"data3_1234567890");

        assert_eq!(buffer.slot_indices.get(key3), Some(&freed_slot));
        assert_eq!(buffer.capacity_slots, 2); // Should not have resized

        // Add fourth item - this should trigger resize
        let key4 = key_map.insert(());
        buffer.update(key4, b"data4_1234567890");

        println!(
            "After resize: capacity={}, next_slot={}, free_slots={:?}",
            buffer.capacity_slots, buffer.next_slot, buffer.free_slots
        );

        assert_eq!(buffer.capacity_slots, 6);
        assert_eq!(buffer.slot_indices.get(key4), Some(&2)); // Should get next_slot=2

        // Verify free slots are correctly set: (2+1)..6 = 3..6 = [3,4,5]
        let mut sorted_free = buffer.free_slots.clone();
        sorted_free.sort();
        assert_eq!(sorted_free, vec![3, 4, 5]);
    }

    #[test]
    fn test_offset_consistency_after_resize() {
        // This test verifies that offset() returns correct values for all keys
        // before and after resize operations
        let mut buffer = create_test_buffer(); // capacity=2, aligned_slice_size=32
        let mut key_map = SlotMap::new();

        // Add initial items and record their offsets
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());

        buffer.update(key1, b"data1___________");
        buffer.update(key2, b"data2___________");

        let offset1_before = buffer.offset(key1).unwrap();
        let offset2_before = buffer.offset(key2).unwrap();
        let slot1 = *buffer.slot_indices.get(key1).unwrap();
        let slot2 = *buffer.slot_indices.get(key2).unwrap();

        // Verify initial offsets are correct: offset = slot * aligned_slice_size
        assert_eq!(offset1_before, slot1 * 32);
        assert_eq!(offset2_before, slot2 * 32);

        println!(
            "Before resize: key1->slot{} offset={}, key2->slot{} offset={}",
            slot1, offset1_before, slot2, offset2_before
        );

        // Add a third item to trigger resize
        let key3 = key_map.insert(());
        buffer.update(key3, b"data3___________");

        // Get all offsets after resize
        let offset1_after = buffer.offset(key1).unwrap();
        let offset2_after = buffer.offset(key2).unwrap();
        let offset3_after = buffer.offset(key3).unwrap();

        let slot1_after = *buffer.slot_indices.get(key1).unwrap();
        let slot2_after = *buffer.slot_indices.get(key2).unwrap();
        let slot3_after = *buffer.slot_indices.get(key3).unwrap();

        println!(
            "After resize: key1->slot{} offset={}, key2->slot{} offset={}, key3->slot{} offset={}",
            slot1_after, offset1_after, slot2_after, offset2_after, slot3_after, offset3_after
        );

        // Verify that existing slots weren't changed during resize
        assert_eq!(slot1, slot1_after, "key1 slot changed during resize!");
        assert_eq!(slot2, slot2_after, "key2 slot changed during resize!");

        // Verify offsets are still correct after resize
        assert_eq!(offset1_after, slot1_after * 32);
        assert_eq!(offset2_after, slot2_after * 32);
        assert_eq!(offset3_after, slot3_after * 32);

        // Verify offsets for pre-resize keys didn't change
        assert_eq!(
            offset1_before, offset1_after,
            "key1 offset changed during resize!"
        );
        assert_eq!(
            offset2_before, offset2_after,
            "key2 offset changed during resize!"
        );

        // Verify all offsets are unique
        let offsets = vec![offset1_after, offset2_after, offset3_after];
        let mut unique_offsets = offsets.clone();
        unique_offsets.sort();
        unique_offsets.dedup();
        assert_eq!(
            offsets.len(),
            unique_offsets.len(),
            "Duplicate offsets detected!"
        );

        // Verify data integrity after resize
        assert_eq!(
            &buffer.raw_data[offset1_after..offset1_after + 16],
            b"data1___________"
        );
        assert_eq!(
            &buffer.raw_data[offset2_after..offset2_after + 16],
            b"data2___________"
        );
        assert_eq!(
            &buffer.raw_data[offset3_after..offset3_after + 16],
            b"data3___________"
        );
    }

    #[test]
    fn test_resize_next_slot_update() {
        // This test verifies that the resize operation correctly updates next_slot
        // to prevent duplicate slot assignments when free_slots is exhausted.

        let mut buffer: DynamicUniformBuffer<TestKey> =
            DynamicUniformBuffer::new(1, 16, Some(32), Some("next_slot_test".to_string()));
        let mut key_map = SlotMap::new();
        let mut keys = Vec::new();

        // Create a scenario that requires proper next_slot management:
        // 1. Start with capacity 1
        // 2. Force resize by adding items
        // 3. Use up all free slots from resize
        // 4. Verify that additional items get correct, non-duplicate slots

        println!("=== Testing Resize Next Slot Update ===");

        // Add first item (uses slot 0, free_slots becomes empty)
        let key1 = key_map.insert(());
        keys.push(key1);
        buffer.update(key1, b"ITEM_01_________");
        println!(
            "Item 1: slot {}, next_slot={}, free_slots={:?}",
            buffer.slot_indices.get(key1).unwrap(),
            buffer.next_slot,
            buffer.free_slots
        );

        // Add second item (forces resize to larger capacity)
        let key2 = key_map.insert(());
        keys.push(key2);
        buffer.update(key2, b"ITEM_02_________");
        println!(
            "Item 2: slot {}, next_slot={}, free_slots={:?}",
            buffer.slot_indices.get(key2).unwrap(),
            buffer.next_slot,
            buffer.free_slots
        );

        // Verify that next_slot is beyond the free slots range to prevent conflicts
        assert!(
            buffer.next_slot > *buffer.free_slots.iter().max().unwrap_or(&0),
            "next_slot should be beyond any free slot to prevent duplicates"
        );

        // Use up the free slots
        let key3 = key_map.insert(());
        keys.push(key3);
        buffer.update(key3, b"ITEM_03_________");
        println!(
            "Item 3: slot {}, next_slot={}, free_slots={:?}",
            buffer.slot_indices.get(key3).unwrap(),
            buffer.next_slot,
            buffer.free_slots
        );

        let key4 = key_map.insert(());
        keys.push(key4);
        buffer.update(key4, b"ITEM_04_________");
        println!(
            "Item 4: slot {}, next_slot={}, free_slots={:?}",
            buffer.slot_indices.get(key4).unwrap(),
            buffer.next_slot,
            buffer.free_slots
        );

        // Add fifth item - this tests the critical case where free_slots is empty
        // and we must use next_slot, which should point to a guaranteed unused slot
        let key5 = key_map.insert(());
        keys.push(key5);
        buffer.update(key5, b"ITEM_05_________");
        println!(
            "Item 5: slot {}, next_slot={}, free_slots={:?}",
            buffer.slot_indices.get(key5).unwrap(),
            buffer.next_slot,
            buffer.free_slots
        );

        // Verify all slots are unique
        let mut slots = std::collections::HashSet::new();
        for (i, &key) in keys.iter().enumerate() {
            let slot = *buffer.slot_indices.get(key).unwrap();
            if !slots.insert(slot) {
                panic!(
                    "Duplicate slot detected: Item {} has slot {} which was already used",
                    i + 1,
                    slot
                );
            }

            // Verify data integrity
            let offset = buffer.offset(key).unwrap();
            let data = &buffer.raw_data[offset..offset + 16];
            let expected = format!("ITEM_{:02}_________", i + 1);
            assert_eq!(
                data,
                expected.as_bytes(),
                "Data corruption detected for item {}",
                i + 1
            );
        }

        println!(
            "SUCCESS: All {} items have unique slots and intact data",
            keys.len()
        );
        println!(
            "Final slots used: {:?}",
            keys.iter()
                .map(|k| buffer.slot_indices.get(*k).unwrap())
                .collect::<Vec<_>>()
        );

        // Verify resize operation correctness:
        // 1. Free slots were correctly populated during resize
        // 2. next_slot was correctly updated to prevent conflicts
        // 3. No duplicate slot assignments occurred

        assert_eq!(keys.len(), 5, "Should have successfully added 5 items");
        assert_eq!(slots.len(), 5, "All 5 items should have unique slots");
    }

    #[test]
    fn test_offset_after_multiple_resizes_and_removals() {
        // Test offset consistency through multiple resize operations and removals
        let mut buffer: DynamicUniformBuffer<TestKey> =
            DynamicUniformBuffer::new(1, 16, Some(32), Some("offset_test".to_string()));
        let mut key_map = SlotMap::new();
        let mut keys_and_data = Vec::new();

        // Add items one by one, triggering multiple resizes
        for i in 0..8 {
            let key = key_map.insert(());
            let data = format!("item_{:02}_data____", i);

            // Store the expected data before calling update
            keys_and_data.push((key, data.clone()));
            buffer.update(key, data.as_bytes());

            // Verify offset is correct for this new key
            let slot = *buffer.slot_indices.get(key).unwrap();
            let expected_offset = slot * 32;
            let actual_offset = buffer.offset(key).unwrap();
            assert_eq!(
                actual_offset, expected_offset,
                "Incorrect offset for key {} at iteration {}",
                i, i
            );

            // Verify all previous keys still have correct offsets and data
            for (j, (prev_key, prev_data)) in keys_and_data.iter().enumerate() {
                if let Some(prev_slot) = buffer.slot_indices.get(*prev_key) {
                    let prev_expected_offset = prev_slot * 32;
                    let prev_actual_offset = buffer.offset(*prev_key).unwrap();
                    assert_eq!(
                        prev_actual_offset, prev_expected_offset,
                        "Key {} offset corrupted at iteration {}",
                        j, i
                    );

                    // Verify data is still intact
                    let data_slice = &buffer.raw_data[prev_actual_offset..prev_actual_offset + 16];
                    assert_eq!(
                        data_slice,
                        prev_data.as_bytes(),
                        "Data corrupted for key {} at iteration {}. Expected: {:?}, Got: {:?}",
                        j,
                        i,
                        prev_data.as_bytes(),
                        data_slice
                    );
                }
            }
        }

        println!(
            "After adding 8 items: capacity={}, slots used: {:?}",
            buffer.capacity_slots,
            buffer.slot_indices.values().collect::<Vec<_>>()
        );

        // Remove some items and verify remaining offsets are still correct
        let keys_to_remove = vec![keys_and_data[1].0, keys_and_data[3].0, keys_and_data[5].0];
        for &key_to_remove in &keys_to_remove {
            buffer.remove(key_to_remove);
        }

        // Verify remaining keys still have correct offsets
        for (i, (key, data)) in keys_and_data.iter().enumerate() {
            if keys_to_remove.contains(key) {
                assert_eq!(
                    buffer.offset(*key),
                    None,
                    "Removed key {} still has offset",
                    i
                );
            } else {
                let slot = *buffer.slot_indices.get(*key).unwrap();
                let expected_offset = slot * 32;
                let actual_offset = buffer.offset(*key).unwrap();
                assert_eq!(
                    actual_offset, expected_offset,
                    "Offset wrong for remaining key {}",
                    i
                );

                // Verify data is still intact
                let data_slice = &buffer.raw_data[actual_offset..actual_offset + 16];
                assert_eq!(
                    data_slice,
                    data.as_bytes(),
                    "Data corrupted for remaining key {}",
                    i
                );
            }
        }

        // Add new items that should reuse freed slots
        for i in 8..12 {
            let key = key_map.insert(());
            let data = format!("new_item_{:02}____", i);
            // Pad to exactly 16 bytes
            let mut data_bytes = data.as_bytes().to_vec();
            data_bytes.resize(16, b'_');

            buffer.update(key, &data_bytes);

            let slot = *buffer.slot_indices.get(key).unwrap();
            let expected_offset = slot * 32;
            let actual_offset = buffer.offset(key).unwrap();
            assert_eq!(
                actual_offset, expected_offset,
                "Incorrect offset for new key {}",
                i
            );

            // Verify the data was written correctly
            let data_slice = &buffer.raw_data[actual_offset..actual_offset + 16];
            assert_eq!(
                data_slice,
                &data_bytes[..],
                "New item data not written correctly"
            );
        }
    }

    #[test]
    fn test_offset_edge_cases_with_manual_state() {
        // Test offset calculation with unusual internal states
        let mut buffer: DynamicUniformBuffer<TestKey> = DynamicUniformBuffer::new(
            3,
            16,
            Some(64),
            Some("edge_offset_test".to_string()), // Different aligned_slice_size
        );
        let mut key_map = SlotMap::new();

        // Test with non-power-of-2 aligned_slice_size to validate offset calculations

        let key1 = key_map.insert(());
        let key2 = key_map.insert(());

        buffer.update(key1, b"test1___________");
        buffer.update(key2, b"test2___________");

        let slot1 = *buffer.slot_indices.get(key1).unwrap();
        let slot2 = *buffer.slot_indices.get(key2).unwrap();

        // With aligned_slice_size=64, offsets should be slot * 64
        assert_eq!(buffer.offset(key1).unwrap(), slot1 * 64);
        assert_eq!(buffer.offset(key2).unwrap(), slot2 * 64);

        // Manipulate internal state to create an unusual scenario
        buffer.free_slots.clear();
        buffer.next_slot = 10; // Way beyond current capacity

        let key3 = key_map.insert(());
        buffer.update(key3, b"test3___________");

        // This should trigger resize and slot 10 should be assigned to key3
        let slot3 = *buffer.slot_indices.get(key3).unwrap();
        assert_eq!(slot3, 10);
        assert_eq!(buffer.offset(key3).unwrap(), 10 * 64);

        // Verify original keys still have correct offsets
        assert_eq!(buffer.offset(key1).unwrap(), slot1 * 64);
        assert_eq!(buffer.offset(key2).unwrap(), slot2 * 64);

        // Verify no offset collisions
        let all_offsets = vec![
            buffer.offset(key1).unwrap(),
            buffer.offset(key2).unwrap(),
            buffer.offset(key3).unwrap(),
        ];
        let mut unique_offsets = all_offsets.clone();
        unique_offsets.sort();
        unique_offsets.dedup();
        assert_eq!(
            all_offsets.len(),
            unique_offsets.len(),
            "Offset collision detected!"
        );
    }

    #[test]
    fn test_offset_boundary_values() {
        // Test offset calculations at boundary values
        let mut buffer: DynamicUniformBuffer<TestKey> = DynamicUniformBuffer::new(
            1,
            1,
            Some(1),
            Some("boundary_offset_test".to_string()), // Minimal sizes
        );
        let mut key_map = SlotMap::new();

        // Test with minimal aligned_slice_size of 1
        let key1 = key_map.insert(());
        buffer.update(key1, &[0x42]);

        assert_eq!(buffer.offset(key1).unwrap(), 0); // First slot should be at offset 0

        // Add more items to trigger resize with minimal size
        let key2 = key_map.insert(());
        buffer.update(key2, &[0x43]);

        let slot1 = *buffer.slot_indices.get(key1).unwrap();
        let slot2 = *buffer.slot_indices.get(key2).unwrap();

        // With aligned_slice_size=1, offsets should just be the slot indices
        assert_eq!(buffer.offset(key1).unwrap(), slot1);
        assert_eq!(buffer.offset(key2).unwrap(), slot2);

        // Verify data is at the correct offsets
        let offset1 = buffer.offset(key1).unwrap();
        let offset2 = buffer.offset(key2).unwrap();

        assert_eq!(buffer.raw_data[offset1], 0x42);
        assert_eq!(buffer.raw_data[offset2], 0x43);
    }

    #[test]
    fn test_new_utility_methods() {
        let mut buffer = create_test_buffer();
        let (_, key1, key2, _) = create_keys();

        // Test is_empty and len
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);

        buffer.update(key1, b"data1___________");
        assert!(!buffer.is_empty());
        assert_eq!(buffer.len(), 1);

        buffer.update(key2, b"data2___________");
        assert_eq!(buffer.len(), 2);

        // Test contains_key
        assert!(buffer.contains_key(key1));
        assert!(buffer.contains_key(key2));

        // Test capacity and free_slots_count
        assert_eq!(buffer.capacity(), 2);
        assert_eq!(buffer.free_slots_count(), 0);

        buffer.remove(key1);
        assert_eq!(buffer.len(), 1);
        assert!(!buffer.contains_key(key1));
        assert!(buffer.contains_key(key2));
        assert_eq!(buffer.free_slots_count(), 1);
    }

    #[test]
    #[should_panic]
    fn test_update_panics_on_oversized_data() {
        let mut buffer: DynamicUniformBuffer<TestKey> =
            DynamicUniformBuffer::new(1, 10, Some(16), None);
        let (_, key1, _, _) = create_keys();

        // This should panic because we're trying to write 11 bytes into a 10-byte slot
        buffer.update(key1, &[0u8; 11]);
    }

    #[test]
    fn test_zero_capacity_initialization() {
        let buffer: DynamicUniformBuffer<TestKey> =
            DynamicUniformBuffer::new(0, 16, Some(32), None);

        assert_eq!(buffer.capacity(), 0);
        assert_eq!(buffer.size(), 0);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_concurrent_operations_simulation() {
        let mut buffer = create_test_buffer();
        let mut key_map = SlotMap::new();
        let mut keys = Vec::new();

        // Simulate concurrent-like access pattern
        for i in 0..10 {
            let key = key_map.insert(());
            keys.push(key);
            let data = format!("data_{:03}________", i); // Exactly 16 bytes (data_000________)
            assert_eq!(data.len(), 16);
            buffer.update(key, data.as_bytes());
        }

        // Remove every third item
        for i in (0..10).step_by(3) {
            buffer.remove(keys[i]);
        }

        // Update remaining items
        for (i, &key) in keys.iter().enumerate() {
            if i % 3 != 0 {
                let data = format!("updt_{:03}________", i); // Exactly 16 bytes
                assert_eq!(data.len(), 16);
                buffer.update(key, data.as_bytes());
            }
        }

        // Add new items to fill gaps
        for i in 10..15 {
            let key = key_map.insert(());
            let data = format!("new__{:03}________", i); // Exactly 16 bytes
            assert_eq!(data.len(), 16);
            buffer.update(key, data.as_bytes());
        }

        // Verify no data corruption
        for (i, &key) in keys.iter().enumerate() {
            if i % 3 != 0 {
                let offset = buffer.offset(key).unwrap();
                let expected = format!("updt_{:03}________", i); // Exactly 16 bytes
                assert_eq!(&buffer.raw_data[offset..offset + 16], expected.as_bytes());
            }
        }
    }

    #[test]
    fn test_no_dangling_slots_after_resize() {
        // This test verifies that our resize approach doesn't permanently lose slots
        // While some slots may be temporarily inaccessible (between old capacity and new capacity),
        // they become accessible in subsequent resizes
        let mut buffer: DynamicUniformBuffer<TestKey> =
            DynamicUniformBuffer::new(2, 16, Some(32), Some("slot_accessibility_test".to_string()));
        let mut key_map = SlotMap::new();

        // Fill initial capacity (slots 0, 1)
        let key1 = key_map.insert(());
        let key2 = key_map.insert(());
        buffer.update(key1, b"data1___________");
        buffer.update(key2, b"data2___________");

        // Trigger first resize by adding key3 (needs slot 2)
        // This calls resize(3), new_cap = max(3,2)*2 = 6
        // free_slots.extend(3..6) adds [3,4,5]
        // next_slot = 6 (beyond capacity to prevent conflicts)
        let key3 = key_map.insert(());
        buffer.update(key3, b"data3___________");

        println!(
            "After first resize: capacity={}, next_slot={}, free_slots={:?}",
            buffer.capacity_slots, buffer.next_slot, buffer.free_slots
        );

        // At this point slots 0,1,2 are allocated, slots 3,4,5 are in free_slots
        // This accounts for all 6 slots in the capacity - no permanent waste

        // Use up available free slots
        let mut all_keys = vec![key1, key2, key3];

        for i in 4..7 {
            // Allocate slots 3,4,5
            let key = key_map.insert(());
            all_keys.push(key);

            let data = format!("data{}___________", i);
            buffer.update(key, data.as_bytes());
        }

        println!(
            "After using free slots: free_slots={:?}, next_slot={}",
            buffer.free_slots, buffer.next_slot
        );

        // Now free_slots is empty, next_slot=6
        assert_eq!(buffer.free_slots.len(), 0, "All free slots should be used");

        // Add another key - this triggers resize because next_slot=6 exceeds capacity-1=5
        let key7 = key_map.insert(());
        buffer.update(key7, b"data7___________");

        println!(
            "After second resize: capacity={}, next_slot={}, allocated_slot={:?}",
            buffer.capacity_slots,
            buffer.next_slot,
            buffer.slot_indices.get(key7)
        );

        // Key insight: The strategy prevents duplicate assignments by sacrificing
        // some temporary inaccessibility. But all slots eventually get used.

        // Let's verify that by continuing to allocate and track slot usage
        let mut max_slot_seen = buffer.slot_indices.values().max().copied().unwrap_or(0);

        // Allocate enough keys to use more slots and verify efficient usage
        for i in 8..20 {
            let key = key_map.insert(());
            let data = format!("data{}__________", i);
            buffer.update(key, data.as_bytes());

            let slot = *buffer.slot_indices.get(key).unwrap();
            max_slot_seen = max_slot_seen.max(slot);
        }

        println!(
            "After extensive allocation: max_slot_used={}, total_keys={}",
            max_slot_seen,
            buffer.slot_indices.len()
        );

        // The resize strategy should not cause excessive slot waste over time
        // While some slots may be skipped temporarily, the overall usage should be efficient
        let total_keys = buffer.slot_indices.len();
        let efficiency = total_keys as f64 / (max_slot_seen + 1) as f64;

        println!(
            "Slot usage efficiency: {:.1}% ({} keys using slots 0-{})",
            efficiency * 100.0,
            total_keys,
            max_slot_seen
        );

        // We should have reasonably efficient slot usage (> 50% is acceptable given the safety tradeoff)
        assert!(
            efficiency > 0.5,
            "Slot usage efficiency should be reasonable: {:.1}%",
            efficiency * 100.0
        );

        println!("SUCCESS: Slot allocation strategy balances safety and efficiency.");
    }
}
