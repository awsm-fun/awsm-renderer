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

/// A descriptor + view budget reservation for a single light, owned
/// transactionally by `Shadows::write_gpu`. The reserved range is
/// `[descriptor_base, descriptor_base + reserved_descriptors)` /
/// `[view_base, view_base + reserved_views)`; the caller commits any
/// prefix `0..=reserved_*` (or drops the alloc to roll back). See
/// [`Shadows::try_alloc_shadow`](super::Shadows::try_alloc_shadow)
/// for the full pattern.
///
/// Not `pub` — this type only escapes the `shadows` module via the
/// transactional API.
#[derive(Clone, Copy, Debug)]
pub(super) struct ShadowAlloc {
    pub descriptor_base: u32,
    pub view_base: u32,
    pub reserved_descriptors: u32,
    pub reserved_views: u32,
}

impl ShadowAlloc {
    /// Free-function flavour of [`super::Shadows::try_alloc_shadow`].
    /// `Shadows::write_gpu` holds mut-borrows of `descriptor_bytes_scratch`
    /// / `view_bytes_scratch` while it packs each light, which blocks
    /// any `&self` method call on `Shadows`. Taking the two counter
    /// values by-value here side-steps that: field reads of
    /// `self.active_descriptor_count` / `self.active_view_count` are
    /// disjoint from the scratch borrows, so split-borrow lets them
    /// through. The convenience method on `Shadows` simply forwards
    /// to this for cleaner test ergonomics.
    pub(super) fn try_new(
        active_descriptor_count: u32,
        active_view_count: u32,
        descriptors: u32,
        views: u32,
        max_descriptors: u32,
        max_views: u32,
    ) -> Option<Self> {
        if active_descriptor_count.saturating_add(descriptors) > max_descriptors {
            return None;
        }
        if active_view_count.saturating_add(views) > max_views {
            return None;
        }
        Some(Self {
            descriptor_base: active_descriptor_count,
            view_base: active_view_count,
            reserved_descriptors: descriptors,
            reserved_views: views,
        })
    }
}

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

#[cfg(test)]
mod tests {
    //! Pure-CPU tests of the shadow-allocation transaction and the
    //! per-light packing budget. No GPU dependencies — these only
    //! exercise the [`ShadowAlloc::try_new`] reservation logic, which
    //! is what gates every per-light descriptor/view write in
    //! `Shadows::write_gpu`. Catches the bug class of "partial
    //! publish", "view-slot mismatch", and "config too aggressive"
    //! before they reach the GPU.

    use super::ShadowAlloc;

    const MAX_D: u32 = 32; // MAX_SHADOW_DESCRIPTORS
    const MAX_V: u32 = 96; // MAX_SHADOW_VIEWS

    #[test]
    fn alloc_succeeds_from_empty() {
        let a = ShadowAlloc::try_new(0, 0, 1, 1, MAX_D, MAX_V).unwrap();
        assert_eq!(a.descriptor_base, 0);
        assert_eq!(a.view_base, 0);
        assert_eq!(a.reserved_descriptors, 1);
        assert_eq!(a.reserved_views, 1);
    }

    #[test]
    fn alloc_advances_bases() {
        // Simulate 8 point lights already packed: each consumes 1
        // descriptor + 6 views.
        let used_d = 8;
        let used_v = 48;
        let a = ShadowAlloc::try_new(used_d, used_v, 1, 6, MAX_D, MAX_V).unwrap();
        assert_eq!(a.descriptor_base, 8);
        assert_eq!(a.view_base, 48);
    }

    #[test]
    fn alloc_rejects_descriptor_overflow() {
        // Already at the cap, asking for 1 more.
        assert!(ShadowAlloc::try_new(MAX_D, 0, 1, 1, MAX_D, MAX_V).is_none());
        // Asking for more than free.
        assert!(ShadowAlloc::try_new(MAX_D - 2, 0, 4, 1, MAX_D, MAX_V).is_none());
        // Exactly at the boundary works.
        assert!(ShadowAlloc::try_new(MAX_D - 4, 0, 4, 4, MAX_D, MAX_V).is_some());
    }

    #[test]
    fn alloc_rejects_view_overflow() {
        // 16 point lights = 96 views = the entire budget. A 17th
        // point light (1 descriptor + 6 views) fits descriptor-wise
        // but blows the view budget.
        assert!(ShadowAlloc::try_new(16, 96, 1, 6, MAX_D, MAX_V).is_none());
        // 15 point lights = 90 views; 16th needs 6 → exactly fits.
        assert!(ShadowAlloc::try_new(15, 90, 1, 6, MAX_D, MAX_V).is_some());
    }

    #[test]
    fn alloc_seventeen_point_lights_scenario() {
        // The bug from review pass #2: 17 point lights would overflow
        // the view buffer. Walk through the sequence the per-frame
        // loop would hit and verify the 17th alloc fails cleanly.
        let mut active_d = 0u32;
        let mut active_v = 0u32;
        for i in 0..17 {
            let alloc = ShadowAlloc::try_new(active_d, active_v, 1, 6, MAX_D, MAX_V);
            if i < 16 {
                let a = alloc.unwrap_or_else(|| panic!("point light #{i} should have fit"));
                active_d = a.descriptor_base + 1;
                active_v = a.view_base + 6;
            } else {
                assert!(
                    alloc.is_none(),
                    "the 17th point light must be rejected — \
                     it would overflow the 96-view buffer"
                );
            }
        }
        // Final state after 16 fit: 16 descriptors, 96 views.
        assert_eq!(active_d, 16);
        assert_eq!(active_v, 96);
    }

    #[test]
    fn alloc_mixed_directional_and_point() {
        let mut active_d = 0u32;
        let mut active_v = 0u32;

        // Directional with 4 cascades: 4 descriptors + 4 views.
        let a = ShadowAlloc::try_new(active_d, active_v, 4, 4, MAX_D, MAX_V).unwrap();
        active_d = a.descriptor_base + 4;
        active_v = a.view_base + 4;

        // 8 point lights: 8 descriptors + 48 views.
        for _ in 0..8 {
            let a = ShadowAlloc::try_new(active_d, active_v, 1, 6, MAX_D, MAX_V).unwrap();
            active_d = a.descriptor_base + 1;
            active_v = a.view_base + 6;
        }
        assert_eq!(active_d, 12);
        assert_eq!(active_v, 52);

        // 20 spot lights = 20 descriptors + 20 views. Descriptor
        // budget runs out first (12 + 20 = 32 = MAX_D — exactly fits).
        for i in 0..21 {
            let alloc = ShadowAlloc::try_new(active_d, active_v, 1, 1, MAX_D, MAX_V);
            if i < 20 {
                let a = alloc.unwrap_or_else(|| panic!("spot #{i} should have fit"));
                active_d = a.descriptor_base + 1;
                active_v = a.view_base + 1;
            } else {
                assert!(alloc.is_none(), "the 21st spot must be rejected");
            }
        }
        assert_eq!(active_d, MAX_D);
        assert_eq!(active_v, 72);
    }

    #[test]
    fn alloc_partial_commit() {
        // Caller reserves 4 cascades but only 3 land (e.g. atlas
        // overflow). The reservation has room for 4; the caller is
        // allowed to "commit" any prefix.
        let alloc = ShadowAlloc::try_new(0, 0, 4, 4, MAX_D, MAX_V).unwrap();
        let landed = 3u32;
        assert!(landed <= alloc.reserved_descriptors);
        assert!(landed <= alloc.reserved_views);
        // Caller would then set
        //   active_descriptor_count = alloc.descriptor_base + landed = 3
        //   active_view_count = alloc.view_base + landed = 3
        // and the unused tail (slot 3) is silently discarded; the next
        // light's try_new starts from active_*_count = 3.
        let next = ShadowAlloc::try_new(3, 3, 1, 1, MAX_D, MAX_V).unwrap();
        assert_eq!(next.descriptor_base, 3);
        assert_eq!(next.view_base, 3);
    }

    #[test]
    fn alloc_saturating_add_doesnt_panic() {
        // u32 overflow regression guard: even with bogus values that
        // would wrap in plain `+`, the saturating add inside try_new
        // returns None cleanly.
        assert!(ShadowAlloc::try_new(u32::MAX - 1, 0, 100, 1, MAX_D, MAX_V).is_none());
        assert!(ShadowAlloc::try_new(0, u32::MAX - 1, 1, 100, MAX_D, MAX_V).is_none());
    }
}
