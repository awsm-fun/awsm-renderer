//! The **NLA (non-linear animation) mixer** — the weighted/additive blending
//! engine that composites [`AnimationClipGroup`](super::clip_group::AnimationClipGroup)s
//! onto a single timeline.
//!
//! The mixer owns one linear timeline (`time`) and a stack of
//! [`AnimationLayer`]s. Each layer is either a [`LayerMode::Replace`] (lerp the
//! accumulator toward the layer sample) or a [`LayerMode::Additive`] (add the
//! layer's delta-from-reference onto the accumulator). Within a layer, each
//! [`AnimationStrip`] places a clip on the timeline at `[start, start + len]`
//! and derives the clip's local time from the mixer timeline (so the mixer —
//! not the per-clip clock — drives looping for the mixer path).
//!
//! The accumulate-then-write composite lives in
//! [`update_animations`](super::animations) which seeds the accumulator from
//! each target's *rest* value (the authored default, captured once) so
//! additive deltas don't accumulate and drift across frames.

use std::collections::HashSet;

use crate::transforms::TransformKey;

use super::clip_group::{AnimationClipKey, AnimationTarget};

/// How a layer composites onto the accumulator.
#[derive(Debug, Clone)]
pub enum LayerMode {
    /// Lerp the accumulator toward the layer's sample by the layer weight.
    Replace,
    /// Add `weight * (layer - reference)` onto the accumulator. The
    /// `reference` is the optional `base_clip` sampled at the same local time,
    /// or the target's rest value when `base_clip` is `None`.
    Additive {
        /// The clip whose pose is the additive reference (the "base" the
        /// additive layer is a delta from). `None` ⇒ use the rest value.
        base_clip: Option<AnimationClipKey>,
    },
}

/// One placement of a clip on the mixer timeline.
#[derive(Debug, Clone)]
pub struct AnimationStrip {
    /// The clip group this strip plays.
    pub clip: AnimationClipKey,
    /// Timeline position where the strip starts.
    pub start: f64,
    /// Timeline length the strip spans (it is active while
    /// `mixer.time ∈ [start, start + len]`).
    pub len: f64,
    /// Local-time scale: `local = (mixer.time - start) / scale`. A `scale` of
    /// `1.0` plays at clip-authored speed; `2.0` plays at half speed.
    pub scale: f64,
    /// When `true` (and the clip has a positive duration) the local time wraps
    /// via `rem_euclid(duration)`; otherwise it clamps to `[0, duration]`.
    pub repeat: bool,
}

impl AnimationStrip {
    /// True when `time` falls inside this strip's active window
    /// `[start, start + len]`.
    pub fn is_active(&self, time: f64) -> bool {
        time >= self.start && time <= self.start + self.len
    }

    /// The clip-local sampling time for this strip at mixer `time`.
    ///
    /// `local = (time - start) / scale`, then wrapped (`repeat`) or clamped to
    /// `[0, duration]`. A non-finite or non-positive `scale` is treated as
    /// `1.0` (defensive — the editor lowering guarantees a sane scale).
    pub fn local_time(&self, time: f64, duration: f64) -> f64 {
        let scale = if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        };
        let local = (time - self.start) / scale;
        if self.repeat && duration > 0.0 {
            local.rem_euclid(duration)
        } else {
            local.clamp(0.0, duration.max(0.0))
        }
    }
}

/// A set of transform keys a layer is restricted to (a "bone mask").
///
/// An empty mask matches *no* transforms. A mask only gates transform targets —
/// non-transform targets (materials, lights, cameras, morphs) are never
/// restricted by a transform mask (see [`Self::contains`]).
#[derive(Debug, Clone, Default)]
pub struct TargetMask {
    /// The transform keys this mask admits.
    pub transforms: HashSet<TransformKey>,
}

impl TargetMask {
    /// Whether `target` passes this mask.
    ///
    /// For a [`AnimationTarget::Transform`] this is membership in the set. For
    /// every other target kind a transform mask does not apply, so it returns
    /// `true` (the layer's non-transform channels are unaffected by the mask).
    pub fn contains(&self, target: AnimationTarget) -> bool {
        match target {
            AnimationTarget::Transform(key) => self.transforms.contains(&key),
            _ => true,
        }
    }
}

/// One layer of the mixer stack: a composite mode + weight + optional mask +
/// the strips that contribute samples.
#[derive(Debug, Clone)]
pub struct AnimationLayer {
    /// Replace or additive compositing.
    pub mode: LayerMode,
    /// Layer blend weight (`0.0` ⇒ no contribution, `1.0` ⇒ full).
    pub weight: f64,
    /// Optional transform mask gating which transform targets this layer
    /// touches.
    pub mask: Option<TargetMask>,
    /// The clip placements feeding this layer.
    pub strips: Vec<AnimationStrip>,
}

impl AnimationLayer {
    /// A new replace layer at full weight with the given strips.
    pub fn new_replace(strips: Vec<AnimationStrip>) -> Self {
        Self {
            mode: LayerMode::Replace,
            weight: 1.0,
            mask: None,
            strips,
        }
    }

    /// A new additive layer (relative to `base_clip`, or rest when `None`) at
    /// full weight with the given strips.
    pub fn new_additive(base_clip: Option<AnimationClipKey>, strips: Vec<AnimationStrip>) -> Self {
        Self {
            mode: LayerMode::Additive { base_clip },
            weight: 1.0,
            mask: None,
            strips,
        }
    }

    /// Whether `target` passes this layer's mask (if any). A layer with no mask
    /// admits every target.
    pub fn admits(&self, target: AnimationTarget) -> bool {
        match &self.mask {
            Some(mask) => mask.contains(target),
            None => true,
        }
    }
}

/// The NLA mixer: a stack of [`AnimationLayer`]s over one linear timeline.
#[derive(Debug, Clone, Default)]
pub struct AnimationMixer {
    /// The composite stack, blended in order (layer 0 first).
    pub layers: Vec<AnimationLayer>,
    time: f64,
}

impl AnimationMixer {
    /// A new empty mixer (timeline at `0.0`, no layers).
    pub fn new() -> Self {
        Self::default()
    }

    /// The current timeline position.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Seek the timeline to an absolute position.
    pub fn set_time(&mut self, time: f64) {
        self.time = time;
    }

    /// Advance the timeline by `dt_seconds` (the unit of strip `start`/`len` and
    /// clip durations; `update_animations` converts the frame's millisecond
    /// delta before calling). The timeline is linear — per-strip `repeat` handles
    /// looping, so the mixer clock never wraps.
    pub fn advance(&mut self, dt_seconds: f64) {
        self.time += dt_seconds;
    }

    /// Drop every layer and reset the timeline to `0.0`.
    pub fn clear(&mut self) {
        self.layers.clear();
        self.time = 0.0;
    }

    /// Whether the mixer has no layers. Drives the single-clip fallback in the
    /// update path (an empty mixer ⇒ play each standalone clip on its own
    /// clock).
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slotmap::{Key, KeyData};

    fn clip_key(idx: u64) -> AnimationClipKey {
        AnimationClipKey::from(KeyData::from_ffi(idx))
    }

    fn tkey(idx: u64) -> TransformKey {
        TransformKey::from(KeyData::from_ffi(idx))
    }

    #[test]
    fn mixer_advance_and_seek() {
        let mut m = AnimationMixer::new();
        assert!(m.is_empty());
        assert_eq!(m.time(), 0.0);
        m.advance(0.5);
        m.advance(0.25);
        assert!((m.time() - 0.75).abs() < 1e-12);
        m.set_time(3.0);
        assert!((m.time() - 3.0).abs() < 1e-12);
        // advance is linear — it never wraps.
        m.advance(100.0);
        assert!((m.time() - 103.0).abs() < 1e-12);
    }

    #[test]
    fn mixer_clear_resets_time_and_layers() {
        let mut m = AnimationMixer::new();
        m.layers.push(AnimationLayer::new_replace(vec![]));
        m.set_time(5.0);
        assert!(!m.is_empty());
        m.clear();
        assert!(m.is_empty());
        assert_eq!(m.time(), 0.0);
    }

    #[test]
    fn strip_active_window() {
        let strip = AnimationStrip {
            clip: clip_key(1),
            start: 2.0,
            len: 3.0,
            scale: 1.0,
            repeat: false,
        };
        assert!(!strip.is_active(1.999));
        assert!(strip.is_active(2.0)); // inclusive start
        assert!(strip.is_active(3.5));
        assert!(strip.is_active(5.0)); // inclusive end (start + len)
        assert!(!strip.is_active(5.001));
    }

    #[test]
    fn strip_local_time_scale() {
        // scale 2.0 ⇒ half speed: at mixer time start+2, local = 1.0.
        let strip = AnimationStrip {
            clip: clip_key(1),
            start: 1.0,
            len: 10.0,
            scale: 2.0,
            repeat: false,
        };
        let local = strip.local_time(3.0, 100.0); // (3-1)/2 = 1.0
        assert!((local - 1.0).abs() < 1e-12);
    }

    #[test]
    fn strip_local_time_repeat_wraps() {
        let strip = AnimationStrip {
            clip: clip_key(1),
            start: 0.0,
            len: 100.0,
            scale: 1.0,
            repeat: true,
        };
        // duration 2.0 ⇒ time 5.0 wraps to rem_euclid(2.0) = 1.0.
        let local = strip.local_time(5.0, 2.0);
        assert!((local - 1.0).abs() < 1e-12, "got {local}");
    }

    #[test]
    fn strip_local_time_clamp_when_not_repeat() {
        let strip = AnimationStrip {
            clip: clip_key(1),
            start: 0.0,
            len: 100.0,
            scale: 1.0,
            repeat: false,
        };
        // Past duration ⇒ clamp to duration.
        assert!((strip.local_time(5.0, 2.0) - 2.0).abs() < 1e-12);
        // Before start (negative raw) ⇒ clamp to 0.
        assert!(strip.local_time(-1.0, 2.0).abs() < 1e-12);
    }

    #[test]
    fn strip_local_time_zero_duration_repeat_is_zero() {
        let strip = AnimationStrip {
            clip: clip_key(1),
            start: 0.0,
            len: 10.0,
            scale: 1.0,
            repeat: true,
        };
        // Zero-duration clip: repeat path is skipped, clamp to [0,0] ⇒ 0.
        assert_eq!(strip.local_time(5.0, 0.0), 0.0);
    }

    #[test]
    fn strip_local_time_bad_scale_falls_back_to_one() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let strip = AnimationStrip {
                clip: clip_key(1),
                start: 0.0,
                len: 10.0,
                scale: bad,
                repeat: false,
            };
            // scale treated as 1.0 ⇒ local == time.
            assert!(
                (strip.local_time(3.0, 100.0) - 3.0).abs() < 1e-12,
                "bad={bad}"
            );
        }
    }

    #[test]
    fn mask_gates_transforms_only() {
        let mut mask = TargetMask::default();
        let in_set = tkey(1);
        let out_set = tkey(2);
        mask.transforms.insert(in_set);

        // Transform in the set passes; out of the set fails.
        assert!(mask.contains(AnimationTarget::Transform(in_set)));
        assert!(!mask.contains(AnimationTarget::Transform(out_set)));

        // Non-transform targets are never restricted by a transform mask.
        let morph = AnimationTarget::Morph(super::super::animations::AnimationMorphKey::Geometry(
            crate::meshes::morphs::GeometryMorphKey::null(),
        ));
        assert!(mask.contains(morph));
    }

    #[test]
    fn empty_mask_matches_no_transform() {
        let mask = TargetMask::default();
        assert!(!mask.contains(AnimationTarget::Transform(tkey(1))));
        // ...but still does not gate non-transforms.
        let cam = AnimationTarget::Camera {
            camera: crate::cameras::CameraKey::null(),
            param: crate::animation::CameraParam::FovY,
        };
        assert!(mask.contains(cam));
    }

    #[test]
    fn layer_admits_respects_optional_mask() {
        let strips = vec![];
        let no_mask = AnimationLayer::new_replace(strips.clone());
        assert!(no_mask.admits(AnimationTarget::Transform(tkey(7))));

        let mut mask = TargetMask::default();
        mask.transforms.insert(tkey(3));
        let masked = AnimationLayer {
            mode: LayerMode::Replace,
            weight: 1.0,
            mask: Some(mask),
            strips,
        };
        assert!(masked.admits(AnimationTarget::Transform(tkey(3))));
        assert!(!masked.admits(AnimationTarget::Transform(tkey(4))));
    }
}
