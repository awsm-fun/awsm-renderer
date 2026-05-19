//! Per-frame shadow records + persistent throttle state.
//!
//! `Shadows::records` holds a `LightShadowRecord` per active casting
//! light, rebuilt every `write_gpu`. `Shadows::throttle` holds a
//! `Vec<ShadowViewThrottle>` per light, indexed in lockstep with the
//! record's `views` and persisted across the per-frame rebuild so a
//! cascade can skip re-rendering when nothing visibly changed.
//!
//! `EvsmDispatchEntry` is the per-cascade work item for the EVSM
//! moment-write + Gaussian blur compute passes; one is queued per
//! EVSM-flagged cascade and consumed by `render_pass::record`.

use glam::Mat4;

/// One queued EVSM moment-write + blur dispatch for the current frame.
/// `descriptor_index` lets the render-pass dispatch fetch the
/// `evsm_rect` (which was patched into the descriptor's `atlas_rect`
/// at write time), but we cache the rects here so the compute-pass
/// loop doesn't need to re-read the descriptor.
#[derive(Clone, Copy, Debug)]
pub struct EvsmDispatchEntry {
    /// Index into `descriptors_uniform` for this cascade.
    pub descriptor_index: u32,
    /// Source rect on `shadow_atlas` in texels (`x, y, w, h`).
    pub pcf_rect: [u32; 4],
    /// Destination rect on `evsm_atlas` in texels (`x, y, w, h`). Also
    /// what receivers sample (UV-converted on read).
    pub evsm_rect: [u32; 4],
    /// Params-buffer slot index for this cascade. Multiplied by
    /// `EVSM_PARAMS_STRIDE` to get the dynamic offset.
    pub params_slot: u32,
}

/// Per-light shadow state recorded each frame.
#[derive(Clone, Debug)]
pub struct LightShadowRecord {
    /// One entry per cascade / face / spot. Phase 2 always has one.
    pub views: Vec<LightShadowView>,
    /// Base index into the descriptor uniform array; the shading
    /// shader fetches `shadow_descriptors[descriptor_base]`.
    pub descriptor_base: u32,
}

/// One renderable shadow view for a light (cascade / face / spot).
#[derive(Clone, Debug)]
pub struct LightShadowView {
    /// Light-space view-projection matrix.
    pub view_projection: Mat4,
    /// Atlas rectangle in texels (x, y, w, h). Used as the viewport
    /// for 2D shadow generation; ignored for cube faces (the cube
    /// face view is rendered at the texture's native resolution).
    pub atlas_rect: [u32; 4],
    /// Cube face layer index when this view targets the cube pool —
    /// `slot * 6 + face_index`. `None` for 2D atlas views.
    pub cube_layer: Option<u32>,
    /// Re-render cadence for this view in frames. `1` means every
    /// frame; the far directional cascade may bump this to 2/4/8 via
    /// `LightShadowParams::far_cascade_update_rate`.
    pub update_period: u64,
    /// Decision flag set by the temporal throttle (Phase 11): `true`
    /// means the render pass should re-render this view, `false`
    /// means the cached atlas tile is still valid for this frame.
    pub should_render: bool,
    /// Global slot index for this view in the per-frame shadow-view
    /// buffer. The render pass uses this as the dynamic offset
    /// multiplier when binding `shadow_view_bind_group`. Set during
    /// `write_gpu` once all views are known.
    pub shadow_view_slot: u32,
}

/// Persistent throttle state per shadow view. Keyed by `(LightKey,
/// view_index)` on `Shadows` so the per-frame `records` rebuild
/// doesn't lose it.
#[derive(Clone, Debug)]
pub struct ShadowViewThrottle {
    /// Frame index at which the view was last rendered. `u64::MAX`
    /// means "never rendered" → force a render this frame.
    pub last_rendered_frame: u64,
    /// Last view-projection we rendered with. Compared each frame so
    /// significant camera / light movement forces an early refresh.
    pub last_view_projection: Mat4,
    /// Last atlas rect we rendered into. If the row-pack allocator
    /// moves this view to a different rect (Phase 13 will re-pack on
    /// caster-set changes), we invalidate the throttle entry so the
    /// stale rect isn't sampled at its new location.
    pub last_atlas_rect: [u32; 4],
}
