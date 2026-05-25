use slotmap::new_key_type;

new_key_type! {
    /// Identifier for a registered line strip.
    pub struct LineKey;
}

/// One `LineSegment` written into the per-line storage buffer (48 bytes).
#[repr(C)]
#[derive(Copy, Clone)]
pub(super) struct GpuLineSegment {
    pub a: [f32; 4],       // .xyz = position A, .w = pad
    pub color_a: [f32; 4], // RGBA at A
    pub b: [f32; 4],       // .xyz = position B, .w = pad
    pub color_b: [f32; 4], // RGBA at B
}

pub(super) const SEGMENT_BYTES: usize = std::mem::size_of::<GpuLineSegment>();

/// 16 bytes — `width_px`, `viewport_w`, `viewport_h`, `_pad`.
pub(super) const LINE_UNIFORM_BYTES: usize = 16;

/// Per-line GPU state.
pub(super) struct LineEntry {
    pub segment_count: u32,
    pub width_px: f32,
    pub depth_test_always: bool,
    pub segment_buffer: web_sys::GpuBuffer,
    pub segment_capacity_bytes: usize,
    pub uniform_buffer: web_sys::GpuBuffer,
    pub bind_group: web_sys::GpuBindGroup,
    /// Phase-2.1 mapped-staging-ring uploaders. Per-entry so the
    /// ring is sized to *this* line's segment/uniform buffer; on
    /// segment_buffer regrow (`segment_capacity_bytes` change), the
    /// uploader's `last_dest_size` mismatch triggers a ring resize
    /// in lockstep. `Mutex` (not `RefCell`) for renderer-wide
    /// consistency — `MappedUploader` transitively owns
    /// `web_sys::GpuBuffer`, which is `!Send`, so the `Mutex` doesn't
    /// *grant* `Sync` to the surrounding entry today.
    pub segments_uploader: std::sync::Mutex<crate::buffer::mapped_uploader::MappedUploader>,
    pub uniform_uploader: std::sync::Mutex<crate::buffer::mapped_uploader::MappedUploader>,
}

/// Packing topology for `positions`/`colors` into `GpuLineSegment` records.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LineTopology {
    /// `positions[i] → positions[i+1]` for each adjacent pair (connected
    /// polyline). N points produce N-1 segments.
    Strip,
    /// `positions[2*i] → positions[2*i+1]` for each pair (disjoint
    /// segments — the same model as line-list topology). N points
    /// produce N/2 segments.
    Segments,
}
