//! Previous-frame copies of GPU buffers, for TAA motion vectors
//! (`docs/plans/taa-upscaling/03-design.md` §2.3).
//!
//! A [`BufferHistory`] mirrors one "current" GPU buffer (world transforms,
//! instance transforms, joint matrices, morph weights) into a *region* of a
//! `prev` buffer with the same layout, so a shader reads last frame's value at
//! the current offset plus the region's base. Everything is a GPU-side copy
//! recorded into the frame's own command encoder:
//!
//! - **Frame start** ([`BufferHistory::record_frame_start`], recorded before
//!   any pass of the frame encoder; this frame's uploads were submitted ahead
//!   of it):
//!   1. a moved / regrown region gets the old region's contents;
//!   2. every *fresh* range gets `prev = current`. A range is fresh when its
//!      data has no previous value: a new allocation (insert, slot reuse
//!      after a remove, a relocation in a buddy allocator), a history reset
//!      (teleport), or the whole buffer when the history was just created.
//!      That is G10: a new object has zero motion, never a recycled slot's
//!      stale matrix.
//! - **Frame end** ([`BufferHistory::record_frame_end`], recorded after every
//!   pass that reads the buffers): one `current -> prev` copy of the whole
//!   buffer, so next frame `prev` holds this frame's values. Skipped when
//!   nothing was uploaded since the last submitted copy (then `prev` already
//!   equals `current`), so a static scene copies nothing.
//! - **After the submit** ([`BufferHistory::frame_submitted`]): the pending
//!   work above is forgotten. A frame that fails before its submit leaves it
//!   pending, so it is recorded again next frame.
//!
//! Uploads (`writeBuffer` and the mapped ring's copies) are submitted before
//! the frame's command buffer (`AwsmRendererWebGpu::submit_commands` flushes
//! the upload encoder first), so "frame start" sees this frame's data and
//! "frame end" leaves it for the next frame.
//!
//! **One storage buffer for the vertex stage.** The previous transforms, joint
//! matrices and morph weights share ONE `prev` buffer ([`MotionHistoryBuffer`],
//! three regions), so the TAA geometry variant binds a single extra storage
//! buffer. The custom-vertex variants already bind nine storage buffers to the
//! vertex stage, and Chrome on Apple GPUs allows ten
//! (`maxStorageBuffersPerShaderStage`). Instance transforms are a vertex
//! buffer and keep a prev buffer of their own ([`BufferHistory::sync_own`]).
//!
//! **Zero cost when off.** A disabled history owns no buffer, records no
//! command and tracks nothing; owners only call into it when it is enabled.

use std::cell::{Cell, RefCell};

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    command::CommandEncoder,
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};

/// Where a history's previous-frame values live: `[base, base + size)` of
/// `buffer`.
#[derive(Clone)]
pub struct HistoryRegion {
    pub buffer: web_sys::GpuBuffer,
    pub base: usize,
    pub size: usize,
}

/// The previous-frame mirror of one GPU buffer. See the module docs.
pub struct BufferHistory {
    label: &'static str,
    enabled: bool,
    region: Option<HistoryRegion>,
    /// The region this one replaced (moved / regrown): its contents are copied
    /// into the new region at frame start.
    retired: RefCell<Option<HistoryRegion>>,
    /// Byte ranges (of the CURRENT buffer) that must get `prev = current` at
    /// the next frame start.
    fresh: RefCell<Vec<(usize, usize)>>,
    /// The whole buffer is fresh (history just created).
    all_fresh: Cell<bool>,
    /// `current` was uploaded since the last recorded end-of-frame copy.
    written: Cell<bool>,
}

impl BufferHistory {
    /// A disabled history.
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            enabled: false,
            region: None,
            retired: RefCell::new(None),
            fresh: RefCell::new(Vec::new()),
            all_fresh: Cell::new(false),
            written: Cell::new(false),
        }
    }

    /// Whether the history is on (TAA). Owners skip all tracking when off.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turns the history on or off. Off forgets the region at once (the
    /// buffer is a JS handle; WebGPU keeps the memory alive for any in-flight
    /// submit). On gets a region at the next sync.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.region = None;
        self.retired.borrow_mut().take();
        self.fresh.borrow_mut().clear();
        self.all_fresh.set(false);
        self.written.set(false);
    }

    /// The previous-frame region, when the history is on and placed.
    pub fn region(&self) -> Option<&HistoryRegion> {
        self.region.as_ref()
    }

    /// `[offset, offset + size)` of the current buffer has no previous
    /// value: give it `prev = current` at the next frame start (G10).
    pub fn mark_fresh(&self, offset: usize, size: usize) {
        if self.enabled && size > 0 {
            self.fresh.borrow_mut().push((offset, size));
        }
    }

    /// The current buffer was uploaded this frame.
    pub fn mark_written(&self) {
        if self.enabled {
            self.written.set(true);
        }
    }

    /// Places the history in `region` (a region of a shared buffer, see
    /// [`MotionHistoryBuffer`]). A previous region's contents are carried over
    /// at frame start; a first placement starts with `prev = current`.
    pub fn assign(&mut self, region: HistoryRegion) {
        match self.region.replace(region) {
            Some(old) => *self.retired.borrow_mut() = Some(old),
            None => self.all_fresh.set(true),
        }
    }

    /// For a history with a buffer of its own (the instance transforms, a
    /// vertex buffer): keeps it the size of the current buffer. Returns `true`
    /// when it was (re)created.
    pub fn sync_own(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        usage: BufferUsage,
        current_size: usize,
    ) -> Result<bool, AwsmCoreError> {
        if !self.enabled || current_size == 0 {
            return Ok(false);
        }
        if self.region.as_ref().is_some_and(|r| current_size <= r.size) {
            return Ok(false);
        }
        let buffer = gpu
            .create_buffer(&BufferDescriptor::new(Some(self.label), current_size, usage).into())?;
        self.assign(HistoryRegion {
            buffer,
            base: 0,
            size: current_size,
        });
        Ok(true)
    }

    /// Frame-start fix-ups (see the module docs). `current` is the owner's
    /// current GPU buffer, `current_size` its byte size.
    pub fn record_frame_start(
        &self,
        encoder: &CommandEncoder,
        current: &web_sys::GpuBuffer,
        current_size: usize,
    ) -> Result<(), AwsmCoreError> {
        let Some(region) = self.region.as_ref() else {
            return Ok(());
        };
        // Nothing is consumed here: the pending work stays pending until
        // `frame_submitted`, so a frame that fails before its submit redoes it
        // next frame (a reused slot must never show its last occupant, G10).
        if let Some(old) = self.retired.borrow().as_ref() {
            copy(
                encoder,
                &old.buffer,
                old.base,
                &region.buffer,
                region.base,
                old.size.min(region.size),
            )?;
        }
        let size = current_size.min(region.size);
        if self.all_fresh.get() {
            return copy(encoder, current, 0, &region.buffer, region.base, size);
        }
        let fresh = self.fresh.borrow();
        if fresh.is_empty() {
            return Ok(());
        }
        for (offset, len) in crate::buffer::helpers::prepare_dirty_ranges(&fresh[..], size) {
            copy(
                encoder,
                current,
                offset,
                &region.buffer,
                region.base + offset,
                len,
            )?;
        }
        Ok(())
    }

    /// The frame whose copies were recorded by [`Self::record_frame_start`] /
    /// [`Self::record_frame_end`] was submitted: forget the work it did. Not
    /// called for a frame that failed before its submit, so that work is
    /// recorded again next frame.
    pub fn frame_submitted(&self) {
        self.retired.borrow_mut().take();
        self.all_fresh.set(false);
        self.fresh.borrow_mut().clear();
        self.written.set(false);
    }

    /// End-of-frame `current -> prev` copy (see the module docs).
    pub fn record_frame_end(
        &self,
        encoder: &CommandEncoder,
        current: &web_sys::GpuBuffer,
        current_size: usize,
    ) -> Result<(), AwsmCoreError> {
        let Some(region) = self.region.as_ref() else {
            return Ok(());
        };
        if !self.written.get() {
            return Ok(());
        }
        copy(
            encoder,
            current,
            0,
            &region.buffer,
            region.base,
            current_size.min(region.size),
        )
    }
}

/// The shared previous-frame buffer of the transforms, joint matrices and
/// morph weights: three regions, each the size of its current buffer, at
/// 256-byte-aligned bases. The geometry shader reads it as one
/// `array<vec4<f32>>` and finds the joint-matrix / morph-weight regions by
/// [`Self::bases_vec4`] (passed in the frame-globals uniform).
#[derive(Default)]
pub struct MotionHistoryBuffer {
    buffer: Option<web_sys::GpuBuffer>,
    /// Region sizes the buffer was laid out for.
    sizes: [usize; 3],
    bases: [usize; 3],
}

impl MotionHistoryBuffer {
    /// The buffer, while TAA is on and something was synced.
    pub fn buffer(&self) -> Option<&web_sys::GpuBuffer> {
        self.buffer.as_ref()
    }

    /// Region bases in `vec4<f32>` units: `[transforms, joint matrices, morph
    /// weights]`.
    pub fn bases_vec4(&self) -> [u32; 3] {
        self.bases.map(|b| (b / 16) as u32)
    }

    /// Drops the buffer (TAA off).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Lays the three histories out for the current buffers' sizes
    /// (`sizes[i]` belongs to `histories[i]`; 0 = that current buffer is
    /// empty, it still gets a minimal region). Reallocates (with headroom)
    /// only when a region outgrew its slot; the histories then carry their
    /// old contents over at frame start. Returns `true` when the buffer was
    /// (re)created, i.e. its bind groups must be rebuilt.
    pub fn sync(
        &mut self,
        gpu: &AwsmRendererWebGpu,
        histories: [&mut BufferHistory; 3],
        sizes: [usize; 3],
    ) -> Result<bool, AwsmCoreError> {
        if !histories[0].enabled() {
            return Ok(false);
        }
        let fits = self.buffer.is_some()
            && sizes
                .iter()
                .zip(self.sizes)
                .all(|(want, have)| *want <= have);
        let placed = histories.iter().all(|h| h.region().is_some());
        if fits && placed {
            return Ok(false);
        }
        let mut new_sizes = self.sizes;
        if !fits {
            for (slot, want) in new_sizes.iter_mut().zip(sizes) {
                if want > *slot {
                    // Headroom so a growing scene does not reallocate every
                    // frame (the current buffers double too).
                    *slot = want.max(256).next_power_of_two();
                }
                *slot = (*slot).max(256);
            }
        }
        let mut bases = [0usize; 3];
        let mut total = 0usize;
        for (base, size) in bases.iter_mut().zip(new_sizes) {
            *base = total;
            total += size.div_ceil(256) * 256;
        }
        let buffer = if fits {
            self.buffer.clone().expect("fits implies a buffer")
        } else {
            gpu.create_buffer(
                &BufferDescriptor::new(
                    Some("TAA previous-frame state"),
                    total,
                    BufferUsage::new().with_storage().with_copy_dst(),
                )
                .into(),
            )?
        };
        for (i, history) in histories.into_iter().enumerate() {
            if !fits || history.region().is_none() {
                history.assign(HistoryRegion {
                    buffer: buffer.clone(),
                    base: bases[i],
                    size: new_sizes[i],
                });
            }
        }
        let recreated = !fits;
        self.buffer = Some(buffer);
        self.sizes = new_sizes;
        self.bases = bases;
        Ok(recreated)
    }
}

/// A GPU copy of `len` bytes from `src[src_offset..]` to `dst[dst_offset..]`.
/// Every owner's records and every region base are 4-byte aligned (WebGPU's
/// copy alignment); a ragged tail is dropped.
fn copy(
    encoder: &CommandEncoder,
    src: &web_sys::GpuBuffer,
    src_offset: usize,
    dst: &web_sys::GpuBuffer,
    dst_offset: usize,
    len: usize,
) -> Result<(), AwsmCoreError> {
    debug_assert!(src_offset % 4 == 0 && dst_offset % 4 == 0);
    let len = len & !3;
    if len == 0 {
        return Ok(());
    }
    encoder.copy_buffer_to_buffer(src, src_offset as u32, dst, dst_offset as u32, len as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_history_tracks_nothing() {
        let h = BufferHistory::new("test");
        h.mark_fresh(0, 64);
        h.mark_written();
        assert!(h.fresh.borrow().is_empty());
        assert!(!h.all_fresh.get());
        assert!(!h.written.get());
        assert!(h.region().is_none());
    }

    #[test]
    fn disabling_forgets_everything() {
        let mut h = BufferHistory::new("test");
        h.set_enabled(true);
        h.mark_fresh(0, 64);
        h.mark_written();
        h.set_enabled(false);
        assert!(h.fresh.borrow().is_empty());
        assert!(!h.written.get());
        h.set_enabled(true);
        assert!(h.fresh.borrow().is_empty());
    }
}
