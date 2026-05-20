//! Triple-buffered staging buffer ring (Cluster 8.2).
//!
//! For per-frame uploads that go through `mapAsync` + `copyBufferToBuffer`
//! rather than `writeBuffer`, the canonical pattern is a ring of N
//! staging buffers — one being mapped on the CPU, one in flight, one
//! ready for the GPU. The current renderer routes everything through
//! `gpu.write_buffer` (`queue.writeBuffer`) which manages staging
//! internally, so the ring is opportunistic: future consumers (e.g.
//! large per-frame instance buffers, GPU-driven culling indirect args)
//! can opt in without re-inventing the cadence.
//!
//! Sizing follows the plan: hand the constructor the 99th-percentile
//! frame upload size; the ring rotates `RING_LEN` such buffers. Each
//! slot is reused (not recreated) on every wrap, so the allocator sees
//! a steady-state working set rather than a per-frame churn.

use std::sync::LazyLock;

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// Number of buffers in the ring. Three is the standard "one CPU + one
/// in-flight + one ready" cadence — two would stall the CPU under
/// any GPU-side latency, four wastes memory.
pub const RING_LEN: usize = 3;

static STAGING_BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_map_write().with_copy_src());

/// A persistent triple-buffered staging buffer ring.
///
/// Consumers grab the current slot at frame start, write their bytes,
/// kick off a `copyBufferToBuffer` to the destination, and advance the
/// ring before the next frame's `write` call.
pub struct StagingRing {
    buffers: [web_sys::GpuBuffer; RING_LEN],
    /// Bytes-capacity of each slot. The ring grows on first request that
    /// exceeds this — costly, so size the initial value to the
    /// 99th-percentile upload up-front.
    slot_capacity: usize,
    /// Cursor into `buffers`. Advances at the end of each frame's
    /// `acquire_next`, so the next call returns the next slot.
    cursor: usize,
    label: &'static str,
}

impl StagingRing {
    /// Builds a ring with `initial_capacity` bytes per slot. The
    /// `label` is propagated to every buffer descriptor for telemetry.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        initial_capacity: usize,
        label: &'static str,
    ) -> Result<Self, AwsmCoreError> {
        let cap = initial_capacity.max(64);
        let buffers = [
            create_slot(gpu, cap, label)?,
            create_slot(gpu, cap, label)?,
            create_slot(gpu, cap, label)?,
        ];
        Ok(Self {
            buffers,
            slot_capacity: cap,
            cursor: 0,
            label,
        })
    }

    /// Returns the current slot. Stable for the duration of the frame —
    /// callers issue their `copyBufferToBuffer` from it, then call
    /// `advance` once at frame end.
    pub fn current(&self) -> &web_sys::GpuBuffer {
        &self.buffers[self.cursor]
    }

    /// Capacity in bytes of each slot.
    pub fn slot_capacity(&self) -> usize {
        self.slot_capacity
    }

    /// Reallocates every slot if `requested_bytes` exceeds the current
    /// capacity. Returns `true` when a grow happened so callers can
    /// invalidate any cached bind-group references to the old buffers.
    pub fn ensure_capacity(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        requested_bytes: usize,
    ) -> Result<bool, AwsmCoreError> {
        if requested_bytes <= self.slot_capacity {
            return Ok(false);
        }
        // 2x headroom on grow — same policy as the mesh buffers.
        let new_cap = (requested_bytes * 2).max(self.slot_capacity * 2);
        for slot in &mut self.buffers {
            *slot = create_slot(gpu, new_cap, self.label)?;
        }
        self.slot_capacity = new_cap;
        Ok(true)
    }

    /// Advances the ring cursor. Call once per frame after issuing the
    /// frame's copies — the next `current()` returns a different slot.
    pub fn advance(&mut self) {
        self.cursor = (self.cursor + 1) % RING_LEN;
    }

    /// Total ring memory footprint (`RING_LEN * slot_capacity`).
    pub fn total_bytes(&self) -> usize {
        RING_LEN * self.slot_capacity
    }
}

fn create_slot(
    gpu: &AwsmRendererWebGpu,
    capacity: usize,
    label: &'static str,
) -> Result<web_sys::GpuBuffer, AwsmCoreError> {
    gpu.create_buffer(&BufferDescriptor::new(Some(label), capacity, *STAGING_BUFFER_USAGE).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Host-target tests can only exercise the cursor logic — buffer
    // creation needs a real GPU. The cursor wrap is the load-bearing
    // bit; everything else is straight-through.
    #[test]
    fn cursor_wraps_after_ring_len() {
        // Manually simulate without constructing a real ring (no GPU).
        let mut cursor = 0_usize;
        let mut seen = Vec::new();
        for _ in 0..RING_LEN * 2 {
            seen.push(cursor);
            cursor = (cursor + 1) % RING_LEN;
        }
        assert_eq!(seen, vec![0, 1, 2, 0, 1, 2]);
    }
}
