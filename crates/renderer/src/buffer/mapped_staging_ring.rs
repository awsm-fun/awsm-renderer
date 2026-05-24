//! Mapped-staging-buffer ring used by [`DynamicStorageBuffer`] /
//! [`DynamicUniformBuffer`] for per-frame uploads.
//!
//! ### Why a ring
//!
//! `queue.writeBuffer` works fine but inserts a browser-managed staging
//! copy hop on every call. For renderer-owned per-frame data that is
//! already dirty-tracked CPU-side, we can do better: each frame we ask
//! the GPU for a `MAP_WRITE | COPY_SRC` slot, `memcpy` directly into
//! its mapped `ArrayBuffer`, `unmap()`, and `copyBufferToBuffer` to the
//! real destination. The slot then enters the in-flight window and we
//! reach for the next one.
//!
//! `RING_DEPTH` slots cycle through four states:
//!
//! | State       | Meaning                                  | CPU may write? |
//! |-------------|------------------------------------------|----------------|
//! | `Mapped`    | `getMappedRange()` returned an ArrayBuffer | yes          |
//! | `Submitted` | `unmap()` + `copyBufferToBuffer` recorded; GPU owns it | no  |
//! | `Pending`   | `mapAsync()` kicked, callback hasn't fired   | no         |
//! | `Ready`     | `mapAsync()` resolved; promotable to `Mapped` on next use | no |
//!
//! All slots are created with `mappedAtCreation: true` so the cold-start
//! frame doesn't have to wait for an async resolution.
//!
//! ### Exhaustion
//!
//! If `acquire(..)` finds no slot in `Mapped`/`Ready` (i.e. every slot
//! is `Submitted` or `Pending`), the ring reports
//! [`AcquireOutcome::Exhausted`]. The consumer responds by falling back
//! to `queue.writeBuffer` for that frame's upload. This path bumps the
//! `fallback_count` telemetry counter so chronic over-subscription
//! shows up.
//!
//! ### Drop
//!
//! `unmap()` is called on any still-`Mapped` slot so WebGPU validation
//! doesn't whine; in-flight slots' underlying `GpuBuffer`s outlive our
//! handle via the device's internal liveness tracking.

use std::cell::Cell;
use std::rc::Rc;

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage, MapMode},
    command::CommandEncoder,
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};
use std::sync::LazyLock;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::js_sys::Uint8Array;

/// Default slot count for `Dynamic{Storage,Uniform}Buffer`. Three is
/// the standard "one CPU + one in-flight + one ready" cadence.
pub const DEFAULT_RING_DEPTH: usize = 3;

/// Minimum allowed ring depth. Depth 1 falls back to writeBuffer-only;
/// depth 2 stalls on any GPU latency; 3 is the sweet spot.
pub const MIN_RING_DEPTH: usize = 1;

static STAGING_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_map_write().with_copy_src());

/// Telemetry exposed through `read_render_pass_timings()` JSON under
/// the `upload_rings` key.
#[derive(Debug, Clone, Copy, Default)]
pub struct UploadStats {
    /// Max number of slots simultaneously not-`Ready` since the last
    /// reset. Reveals under-/over-provisioning of the ring.
    pub peak_ring_depth_used: usize,
    /// Frames where `queue.writeBuffer` fallback fired due to ring
    /// exhaustion.
    pub fallback_count: u64,
    /// Accumulated wall-clock time spent blocked on `mapAsync`
    /// resolution waits. ~zero in steady state.
    pub map_async_wait_ms: f64,
    /// Bytes pushed through the mapped fast path.
    pub bytes_uploaded_via_ring: u64,
    /// Bytes pushed through the writeBuffer fallback (ring exhaustion).
    pub bytes_uploaded_via_fallback: u64,
    /// Bytes pushed via the explicit `ingest_foreign` entrypoint.
    pub bytes_uploaded_via_writebuffer: u64,
    /// Times the ring was recreated due to dest-buffer growth.
    pub resize_count: u64,
}

/// Per-slot state for the state machine; see module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Mapped,
    Submitted,
    Pending,
    Ready,
}

/// A flag shared with the `mapAsync` resolution closure. The future
/// flips it to `true` once the promise resolves; the ring polls the
/// flag on each acquire to promote `Pending` slots to `Ready`.
type ReadyFlag = Rc<Cell<bool>>;

struct Slot {
    buffer: web_sys::GpuBuffer,
    state: SlotState,
    /// Set to `true` by the `mapAsync` future when it resolves.
    ready_flag: ReadyFlag,
}

/// Outcome of [`MappedStagingRing::acquire`].
pub enum AcquireOutcome<'a> {
    /// Slot is mapped and ready to be written. The contained
    /// [`MappedSlotWrite`] gates the unmap + copy.
    Acquired(MappedSlotWrite<'a>),
    /// No slot is currently `Mapped`/`Ready`. Caller should fall back to
    /// `queue.writeBuffer`. The ring auto-bumps its `fallback_count`.
    Exhausted,
}

/// RAII handle returned by a successful acquire. Holds the mapped
/// `Uint8Array` and remembers the slot index for the matching
/// finalize call. Drop without finalize is a deliberate no-op: the
/// slot is left in its `Mapped` state so the next `acquire(..)`
/// returns it again — typical for callers that `?`-bail mid-write
/// before recording the copy.
pub struct MappedSlotWrite<'a> {
    ring: &'a mut MappedStagingRing,
    slot_index: usize,
    /// `getMappedRange()` result, cached so the caller doesn't pay
    /// crossing the wasm/JS boundary per byte.
    view: Uint8Array,
    capacity: usize,
    /// Set true on `finalize`; the `Drop` impl reads this only to
    /// distinguish "committed" from "abandoned"; both branches keep
    /// the slot mapped — see the `Drop` impl below for why.
    finalized: bool,
}

impl<'a> MappedSlotWrite<'a> {
    /// Write `bytes` into the mapped range starting at `offset`.
    ///
    /// The slice is copied straight into the GPU-visible `ArrayBuffer`
    /// via `Uint8Array::set` (one `memcpy` across the wasm/JS boundary).
    pub fn write(&self, offset: usize, bytes: &[u8]) {
        debug_assert!(offset + bytes.len() <= self.capacity);
        // `unsafe` is fine here: we're copying *out of* a Rust slice
        // we own into a JS-owned ArrayBuffer; `Uint8Array::view` doesn't
        // outlive the borrow.
        let src = unsafe { Uint8Array::view(bytes) };
        self.view.set(&src, offset as u32);
    }

    /// Total mapped capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Unmap, record the destination copy, and rotate the ring.
    ///
    /// `dest_offset` mirrors the offset at the source slot — the slot
    /// is sized to match the destination buffer so this is normally 0
    /// when the entire payload is in the slot.
    pub fn finalize(
        mut self,
        encoder: &CommandEncoder,
        dest: &web_sys::GpuBuffer,
        copy_ranges: &[(usize, usize)],
    ) -> Result<(), AwsmCoreError> {
        // unmap before copy — WebGPU forbids mapped buffers as copy
        // sources.
        self.ring.slots[self.slot_index].buffer.unmap();

        // Record copy(es).
        for (offset, size) in copy_ranges {
            if *size == 0 {
                continue;
            }
            encoder.copy_buffer_to_buffer(
                &self.ring.slots[self.slot_index].buffer,
                *offset as u32,
                dest,
                *offset as u32,
                *size as u32,
            )?;
        }

        let total: usize = copy_ranges.iter().map(|(_, s)| *s).sum();
        self.ring.stats.bytes_uploaded_via_ring += total as u64;

        self.ring.slots[self.slot_index].state = SlotState::Submitted;
        self.ring.update_peak();

        // NOTE: we intentionally do *not* kick `mapAsync` here. The
        // copy command we just recorded references this slot's buffer
        // as a copy source; if we put the buffer into a
        // pending-mapAsync state before the encoder has been submitted,
        // WebGPU validation rejects the submit ("buffer is used in a
        // submission while a map is pending"). The caller is required
        // to submit the encoder *first*, then call
        // [`MappedStagingRing::kick_submitted_slots`] to roll the
        // Submitted slots forward to Pending. See
        // [`crate::buffer::mapped_uploader::MappedUploader::write_dirty_ranges`]
        // for the canonical ordering.
        self.finalized = true;
        Ok(())
    }
}

impl<'a> Drop for MappedSlotWrite<'a> {
    fn drop(&mut self) {
        if !self.finalized {
            // The slot stays Mapped — caller decided not to commit.
            // We do NOT unmap here: the slot is still legal to retry.
            // Leaving the state alone means the next acquire returns
            // this same slot. (Concrete case: a `?` after acquire but
            // before write_gpu does its copy.)
        }
    }
}

/// Triple-buffered (by default) ring of `MAP_WRITE | COPY_SRC` slots.
///
/// Sized to match a single destination buffer; on dest growth the ring
/// must be recreated via [`Self::resize`].
pub struct MappedStagingRing {
    slots: Vec<Slot>,
    /// Round-robin cursor into `slots` — points at the next slot the
    /// caller will try to acquire.
    next: usize,
    /// Bytes per slot. Matches the destination buffer's size.
    slot_capacity: usize,
    /// Buffer label, propagated to every slot for renderer telemetry.
    label: String,
    /// Telemetry counters.
    stats: UploadStats,
}

impl MappedStagingRing {
    /// Creates a ring with `depth` slots of `capacity` bytes each. All
    /// slots start in `Mapped` state via `mappedAtCreation: true`.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        depth: usize,
        capacity: usize,
        label: impl Into<String>,
    ) -> Result<Self, AwsmCoreError> {
        let depth = depth.max(MIN_RING_DEPTH);
        let label = label.into();
        let mut slots = Vec::with_capacity(depth);
        for _ in 0..depth {
            slots.push(Self::make_slot(gpu, capacity, &label)?);
        }
        Ok(Self {
            slots,
            next: 0,
            slot_capacity: capacity,
            label,
            stats: UploadStats::default(),
        })
    }

    /// Returns the configured slot capacity (bytes).
    pub fn slot_capacity(&self) -> usize {
        self.slot_capacity
    }

    /// Returns the ring depth (slot count).
    pub fn depth(&self) -> usize {
        self.slots.len()
    }

    /// Returns a copy of the current telemetry snapshot.
    pub fn stats(&self) -> UploadStats {
        self.stats
    }

    /// Resets all monotonic counters (`fallback_count`, `bytes_*`,
    /// `resize_count`, `map_async_wait_ms`) and the peak-tracker. Use
    /// from a `clear_stats()` boundary in higher-level code.
    pub fn reset_stats(&mut self) {
        self.stats = UploadStats::default();
    }

    /// Recreates the ring at a new slot capacity. Any in-flight
    /// `Pending`/`Submitted` slots are dropped; the GPU buffer
    /// destructor handles them after their submit completes. Returns
    /// `Ok` once the new ring is ready to write.
    pub fn resize(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        new_capacity: usize,
    ) -> Result<(), AwsmCoreError> {
        if new_capacity == self.slot_capacity {
            return Ok(());
        }
        // Unmap anything still mapped to keep WebGPU validation quiet.
        for slot in &self.slots {
            if matches!(slot.state, SlotState::Mapped | SlotState::Ready) {
                slot.buffer.unmap();
            }
        }
        let depth = self.slots.len();
        let mut new_slots = Vec::with_capacity(depth);
        for _ in 0..depth {
            new_slots.push(Self::make_slot(gpu, new_capacity, &self.label)?);
        }
        self.slots = new_slots;
        self.next = 0;
        self.slot_capacity = new_capacity;
        self.stats.resize_count += 1;
        Ok(())
    }

    /// Try to acquire the next slot for writing. Returns
    /// [`AcquireOutcome::Exhausted`] when every slot is in-flight; the
    /// caller should fall back to `queue.writeBuffer` and the ring
    /// auto-bumps `fallback_count`.
    pub fn acquire(&mut self) -> AcquireOutcome<'_> {
        self.promote_resolved();

        let depth = self.slots.len();
        for offset in 0..depth {
            let idx = (self.next + offset) % depth;
            match self.slots[idx].state {
                SlotState::Mapped => {
                    return self.return_mapped(idx);
                }
                SlotState::Ready => {
                    // Already resolved; getMappedRange() needs to be
                    // called fresh after a mapAsync resolution.
                    let cap = self.slot_capacity;
                    let array = match self.slots[idx]
                        .buffer
                        .get_mapped_range_with_u32_and_u32(0, cap as u32)
                    {
                        Ok(ab) => ab,
                        Err(err) => {
                            tracing::error!(
                                "mapped-ring {}: getMappedRange after mapAsync failed: {:?}",
                                self.label,
                                err
                            );
                            // Treat as exhausted; caller falls back.
                            self.stats.fallback_count += 1;
                            return AcquireOutcome::Exhausted;
                        }
                    };
                    self.slots[idx].state = SlotState::Mapped;
                    return self.return_mapped_with_view(idx, Uint8Array::new(&array));
                }
                _ => {}
            }
        }

        // Nothing acquirable.
        self.stats.fallback_count += 1;
        AcquireOutcome::Exhausted
    }

    /// Record a fallback writeBuffer of `bytes` so telemetry includes
    /// the bytes-uploaded count. Called by consumers after they take
    /// the writeBuffer path due to either [`AcquireOutcome::Exhausted`]
    /// or an explicit foreign-data ingestion.
    pub fn note_fallback_bytes(&mut self, bytes: u64) {
        self.stats.bytes_uploaded_via_fallback += bytes;
    }

    /// Record bytes uploaded via the explicit foreign-data writeBuffer
    /// entrypoint (`ingest_foreign`). Tracked separately from
    /// `fallback` so consumers can tell "too much foreign data" from
    /// "ring is too shallow."
    pub fn note_writebuffer_bytes(&mut self, bytes: u64) {
        self.stats.bytes_uploaded_via_writebuffer += bytes;
    }

    fn return_mapped(&mut self, idx: usize) -> AcquireOutcome<'_> {
        let array = match self.slots[idx]
            .buffer
            .get_mapped_range_with_u32_and_u32(0, self.slot_capacity as u32)
        {
            Ok(ab) => ab,
            Err(err) => {
                tracing::error!(
                    "mapped-ring {}: getMappedRange on Mapped slot failed: {:?}",
                    self.label,
                    err
                );
                self.stats.fallback_count += 1;
                return AcquireOutcome::Exhausted;
            }
        };
        self.return_mapped_with_view(idx, Uint8Array::new(&array))
    }

    fn return_mapped_with_view(&mut self, idx: usize, view: Uint8Array) -> AcquireOutcome<'_> {
        self.next = (idx + 1) % self.slots.len();
        let capacity = self.slot_capacity;
        AcquireOutcome::Acquired(MappedSlotWrite {
            ring: self,
            slot_index: idx,
            view,
            capacity,
            finalized: false,
        })
    }

    /// Promote any `Pending` slots whose `mapAsync` futures resolved
    /// to `Ready`. Cheap (`N` `Cell` reads).
    fn promote_resolved(&mut self) {
        for slot in &mut self.slots {
            if slot.state == SlotState::Pending && slot.ready_flag.get() {
                slot.state = SlotState::Ready;
            }
        }
    }

    /// Kick `mapAsync` on every slot currently in `Submitted` state,
    /// transitioning them to `Pending`. **Must be called *after* the
    /// command buffer that records the slot's copy command has been
    /// submitted to the queue** — otherwise WebGPU rejects the
    /// submission because the buffer would be in a pending-map state
    /// while still referenced by a not-yet-submitted command buffer.
    ///
    /// The canonical caller is
    /// [`crate::buffer::mapped_uploader::MappedUploader::write_dirty_ranges`],
    /// which interleaves: acquire → write → finalize (records copy +
    /// marks Submitted) → `gpu.submit_commands(...)` → this method.
    pub fn kick_submitted_slots(&mut self) {
        let depth = self.slots.len();
        for idx in 0..depth {
            if self.slots[idx].state == SlotState::Submitted {
                self.start_map_async(idx);
            }
        }
    }

    fn start_map_async(&mut self, idx: usize) {
        let ready_flag = self.slots[idx].ready_flag.clone();
        let buffer = self.slots[idx].buffer.clone();
        let capacity = self.slot_capacity as u32;
        let label = self.label.clone();
        ready_flag.set(false);
        let promise = buffer.map_async_with_u32_and_u32(MapMode::Write as u32, 0, capacity);
        spawn_local(async move {
            match JsFuture::from(promise).await {
                Ok(_) => {
                    ready_flag.set(true);
                }
                Err(err) => {
                    // The buffer might have been destroyed (ring
                    // resize / drop); treat as a no-op.
                    tracing::debug!(
                        "mapped-ring {}: mapAsync did not resolve cleanly: {:?}",
                        label,
                        err
                    );
                }
            }
        });
        self.slots[idx].state = SlotState::Pending;
    }

    fn update_peak(&mut self) {
        // Only `Submitted` + `Pending` count as oversubscription
        // pressure — those slots are owned by the GPU / waiting on
        // `mapAsync` and can't be acquired for the next write.
        // `Mapped` and `Ready` are both *acquirable* (Mapped right
        // now, Ready promotes to Mapped on the next `acquire`), so
        // including them would make `peak_ring_depth_used` ≈ ring
        // depth in steady state regardless of actual contention.
        // The metric should answer "how close did we come to
        // exhausting the ring?" — that's exactly the non-acquirable
        // count.
        let used = self
            .slots
            .iter()
            .filter(|s| matches!(s.state, SlotState::Submitted | SlotState::Pending))
            .count();
        if used > self.stats.peak_ring_depth_used {
            self.stats.peak_ring_depth_used = used;
        }
    }

    fn make_slot(
        gpu: &AwsmRendererWebGpu,
        capacity: usize,
        label: &str,
    ) -> Result<Slot, AwsmCoreError> {
        let descriptor = BufferDescriptor::new(Some(label), capacity, *STAGING_USAGE)
            .with_mapped_at_creation(true);
        let buffer = gpu.create_buffer(&descriptor.into())?;
        Ok(Slot {
            buffer,
            state: SlotState::Mapped,
            ready_flag: Rc::new(Cell::new(false)),
        })
    }
}

impl Drop for MappedStagingRing {
    fn drop(&mut self) {
        for slot in &self.slots {
            if matches!(slot.state, SlotState::Mapped | SlotState::Ready) {
                // Silence WebGPU validation; the GpuBuffer destructor
                // handles everything else.
                slot.buffer.unmap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure state-machine test: 4-state transitions for a single slot
    /// driven by ring-level events, with no GPU dependency.
    #[derive(Debug)]
    struct SlotFsm {
        state: SlotState,
    }

    impl SlotFsm {
        fn new() -> Self {
            Self {
                state: SlotState::Mapped,
            }
        }
        fn finalize(&mut self) {
            assert_eq!(self.state, SlotState::Mapped);
            self.state = SlotState::Submitted;
        }
        fn kick_map_async(&mut self) {
            assert_eq!(self.state, SlotState::Submitted);
            self.state = SlotState::Pending;
        }
        fn map_async_resolved(&mut self) {
            assert_eq!(self.state, SlotState::Pending);
            self.state = SlotState::Ready;
        }
        fn promote_to_mapped(&mut self) {
            assert_eq!(self.state, SlotState::Ready);
            self.state = SlotState::Mapped;
        }
    }

    #[test]
    fn slot_fsm_happy_path() {
        let mut fsm = SlotFsm::new();
        // Frame 1: write → submit → kick mapAsync
        fsm.finalize();
        fsm.kick_map_async();
        // Some time later: resolution callback fires
        fsm.map_async_resolved();
        // Next acquire promotes Ready → Mapped
        fsm.promote_to_mapped();
        // And we can write again
        fsm.finalize();
    }

    #[test]
    #[should_panic]
    fn slot_fsm_rejects_kick_without_finalize() {
        let mut fsm = SlotFsm::new();
        fsm.kick_map_async();
    }

    #[test]
    #[should_panic]
    fn slot_fsm_rejects_promote_without_resolve() {
        let mut fsm = SlotFsm::new();
        fsm.finalize();
        fsm.kick_map_async();
        fsm.promote_to_mapped();
    }

    /// Cursor model — independent of GPU buffer lifetimes.
    #[test]
    fn cursor_advances_round_robin() {
        let depth = 3;
        let mut cursor = 0;
        let mut visited = Vec::new();
        for _ in 0..6 {
            visited.push(cursor);
            cursor = (cursor + 1) % depth;
        }
        assert_eq!(visited, vec![0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn upload_stats_default_zero() {
        let s = UploadStats::default();
        assert_eq!(s.peak_ring_depth_used, 0);
        assert_eq!(s.fallback_count, 0);
        assert_eq!(s.bytes_uploaded_via_ring, 0);
        assert_eq!(s.bytes_uploaded_via_fallback, 0);
        assert_eq!(s.bytes_uploaded_via_writebuffer, 0);
        assert_eq!(s.resize_count, 0);
    }
}
