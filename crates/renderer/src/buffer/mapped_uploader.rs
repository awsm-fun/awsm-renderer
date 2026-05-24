//! [`MappedUploader`] — call-site companion that turns
//! dirty-range writes into mapped-buffer uploads.
//!
//! ### Why a wrapper, not "inside `DynamicStorageBuffer`"
//!
//! The original Phase 2.1 spec called for integrating
//! [`MappedStagingRing`] directly into
//! [`crate::buffer::dynamic_storage::DynamicStorageBuffer`] /
//! [`crate::buffer::dynamic_uniform::DynamicUniformBuffer`]. That
//! would have required moving `gpu_buffer: web_sys::GpuBuffer`
//! ownership out of every call site (`Transforms`, `Materials`,
//! `Instances`, etc.) into the buffer type — invasive, and the buffer
//! types are also used in unit tests that have no GPU. Threading
//! `Option<&AwsmRendererWebGpu>` through every constructor / resize
//! path bloats the API surface for negligible win.
//!
//! Instead, `MappedUploader` is a thin per-call-site companion:
//!
//! - Holds a [`MappedStagingRing`] sized to the call site's GPU buffer.
//! - Mirrors resize lifecycle: when the dest grows, the ring grows.
//! - Drops into existing `write_gpu` paths as a one-line swap for
//!   [`crate::buffer::helpers::write_buffer_with_dirty_ranges`].
//! - Exposes [`UploadStats`] for the renderer-wide telemetry rollup.
//!
//! Spec deviation rationale: the spirit of Phase 2.1 ("one canonical
//! mapped-write upload path for renderer-owned per-frame data") is
//! preserved; only the placement of the ring moves from "inside
//! Dynamic" to "alongside Dynamic at the call site." Migration of
//! every per-frame writeBuffer site is still in-scope.

use awsm_renderer_core::{error::AwsmCoreError, renderer::AwsmRendererWebGpu};

use crate::buffer::mapped_staging_ring::{
    AcquireOutcome, MappedStagingRing, UploadStats, DEFAULT_RING_DEPTH,
};

/// Per-call-site mapped-upload companion. See module docs.
pub struct MappedUploader {
    ring: Option<MappedStagingRing>,
    /// Last-known dest-buffer size in bytes. Used to detect dest
    /// resize so the ring can grow in lockstep.
    last_dest_size: usize,
    ring_depth: usize,
    label: String,
    /// Stash of stats from the previous ring (so `resize_count`,
    /// `bytes_*`, etc. survive a ring rebuild). Aggregated with the
    /// live ring's stats by [`Self::stats`].
    stashed_stats: UploadStats,
}

impl MappedUploader {
    /// Creates an uploader. The ring is built lazily on the first
    /// `write_dirty_ranges` call once we know the dest size.
    pub fn new(label: impl Into<String>) -> Self {
        Self::with_ring_depth(label, DEFAULT_RING_DEPTH)
    }

    /// Creates an uploader with a non-default ring depth.
    pub fn with_ring_depth(label: impl Into<String>, depth: usize) -> Self {
        Self {
            ring: None,
            last_dest_size: 0,
            ring_depth: depth,
            label: label.into(),
            stashed_stats: UploadStats::default(),
        }
    }

    /// Aggregated upload stats (live ring + stashed prior-ring totals).
    pub fn stats(&self) -> UploadStats {
        let mut s = self.stashed_stats;
        if let Some(ring) = &self.ring {
            let live = ring.stats();
            s.peak_ring_depth_used = s.peak_ring_depth_used.max(live.peak_ring_depth_used);
            s.fallback_count += live.fallback_count;
            s.map_async_wait_ms += live.map_async_wait_ms;
            s.bytes_uploaded_via_ring += live.bytes_uploaded_via_ring;
            s.bytes_uploaded_via_fallback += live.bytes_uploaded_via_fallback;
            s.bytes_uploaded_via_writebuffer += live.bytes_uploaded_via_writebuffer;
            s.resize_count += live.resize_count;
        }
        s
    }

    /// Reset all monotonic counters and the peak tracker.
    pub fn reset_stats(&mut self) {
        self.stashed_stats = UploadStats::default();
        if let Some(ring) = &mut self.ring {
            ring.reset_stats();
        }
    }

    /// Write `ranges` from `raw_data` to `dest` via the ring, or fall
    /// back to `queue.writeBuffer` on exhaustion.
    ///
    /// `dest_size` is the current size in bytes of the destination
    /// buffer — used to (re)build the ring on growth.
    ///
    /// Ranges are `(offset, size)` pairs; sizes/offsets must be
    /// 4-byte aligned (the [`crate::buffer::dynamic_storage`] dirty
    /// tracker enforces this).
    pub fn write_dirty_ranges(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        dest: &web_sys::GpuBuffer,
        dest_size: usize,
        raw_data: &[u8],
        ranges: &[(usize, usize)],
    ) -> Result<(), AwsmCoreError> {
        if ranges.is_empty() || raw_data.is_empty() {
            return Ok(());
        }

        // Sync ring with dest size (lazy-create or resize).
        self.ensure_ring(gpu, dest_size)?;

        let ring = self
            .ring
            .as_mut()
            .expect("ring is created by ensure_ring above");

        // The MappedUploader owns a single ephemeral CommandEncoder per
        // call so the copy_buffer_to_buffer command can be submitted
        // immediately — this avoids threading a shared encoder through
        // every renderer subsystem's `write_gpu(..)` signature (14 call
        // sites). Submit cost is small (a few µs); we trade a handful
        // of per-frame submits for keeping the existing
        // `(logging, gpu, bind_groups)` signature stable everywhere.
        let encoder = gpu.create_command_encoder(Some(&self.label));

        let exhausted = match ring.acquire() {
            AcquireOutcome::Acquired(slot) => {
                // Memcpy each dirty range into the matching slot offset.
                for (off, sz) in ranges {
                    let end = off.saturating_add(*sz).min(raw_data.len());
                    if *off < end {
                        slot.write(*off, &raw_data[*off..end]);
                    }
                }
                slot.finalize(&encoder, dest, ranges)?;
                gpu.submit_commands(&encoder.finish());
                false
            }
            AcquireOutcome::Exhausted => true,
        };

        if exhausted {
            // Fallback: writeBuffer the dirty ranges. The ring's
            // `acquire()` already bumped `fallback_count`; we just
            // tally bytes here.
            let mut total = 0u64;
            for (off, sz) in ranges {
                if *sz == 0 {
                    continue;
                }
                let end = off.saturating_add(*sz).min(raw_data.len());
                if *off < end {
                    gpu.write_buffer(dest, Some(*off), &raw_data[*off..end], None, None)?;
                    total += (end - off) as u64;
                }
            }
            ring.note_fallback_bytes(total);
        }

        Ok(())
    }

    /// Foreign-bytes ingestion path (Phase 2.1 `ingest_foreign`).
    /// Bypasses the ring; the mapped path doesn't help when the source
    /// bytes already live in a JS-side `ArrayBuffer` / Rust `Vec` —
    /// the memcpy is the same either way.
    pub fn ingest_foreign(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        dest: &web_sys::GpuBuffer,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), AwsmCoreError> {
        gpu.write_buffer(dest, Some(offset), bytes, None, None)?;
        if let Some(ring) = &mut self.ring {
            ring.note_writebuffer_bytes(bytes.len() as u64);
        } else {
            self.stashed_stats.bytes_uploaded_via_writebuffer += bytes.len() as u64;
        }
        Ok(())
    }

    /// Drop the current ring. Useful when the consumer wants to free
    /// the staging memory (e.g. a `clear()` on the parent buffer that
    /// drops everything back to zero size).
    pub fn release_ring(&mut self) {
        if let Some(ring) = self.ring.take() {
            // Roll live stats into the stash so accumulated bytes
            // don't disappear.
            let live = ring.stats();
            self.stashed_stats.peak_ring_depth_used = self
                .stashed_stats
                .peak_ring_depth_used
                .max(live.peak_ring_depth_used);
            self.stashed_stats.fallback_count += live.fallback_count;
            self.stashed_stats.map_async_wait_ms += live.map_async_wait_ms;
            self.stashed_stats.bytes_uploaded_via_ring += live.bytes_uploaded_via_ring;
            self.stashed_stats.bytes_uploaded_via_fallback += live.bytes_uploaded_via_fallback;
            self.stashed_stats.bytes_uploaded_via_writebuffer +=
                live.bytes_uploaded_via_writebuffer;
            self.stashed_stats.resize_count += live.resize_count;
        }
        self.last_dest_size = 0;
    }

    fn ensure_ring(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        dest_size: usize,
    ) -> Result<(), AwsmCoreError> {
        if dest_size == 0 {
            return Ok(());
        }
        match self.ring.as_mut() {
            None => {
                let ring = MappedStagingRing::new(
                    gpu,
                    self.ring_depth,
                    dest_size,
                    self.label.clone(),
                )?;
                self.ring = Some(ring);
                self.last_dest_size = dest_size;
            }
            Some(ring) if dest_size != self.last_dest_size => {
                ring.resize(gpu, dest_size)?;
                self.last_dest_size = dest_size;
            }
            Some(_) => {}
        }
        Ok(())
    }
}
