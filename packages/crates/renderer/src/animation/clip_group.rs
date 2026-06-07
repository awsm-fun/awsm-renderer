//! A **named clip group** — a set of channel-samplers that share one clock
//! (duration / loop / speed / direction), advancing & wrapping in sync. This is
//! the runtime form the editor's authored "Clip" lowers to (one Clip → one
//! `AnimationClipGroup`), formalizing the shared-clock semantics the loose
//! per-channel [`AnimationPlayer`](super::player::AnimationPlayer)s lack.

use slotmap::new_key_type;

use crate::cameras::CameraKey;
use crate::lights::LightKey;
use crate::materials::MaterialKey;
use crate::transforms::TransformKey;

use super::{
    animations::AnimationMorphKey,
    data::AnimationData,
    player::{AnimationPlayDirection, AnimationState},
    sampler::AnimationSampler,
};

pub use super::player::AnimationLoopStyle;

new_key_type! {
    /// SlotMap key for a named clip group.
    pub struct AnimationClipKey;
}

/// What a single channel of a clip drives — a node transform, a mesh morph,
/// a material uniform or built-in factor, a light, or a camera. Keys are
/// `Copy`, so the whole target is `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimationTarget {
    /// A node's local transform (translation/rotation/scale).
    Transform(TransformKey),
    /// A mesh morph-weight set (geometry or material).
    Morph(AnimationMorphKey),
    /// A single uniform slot of a custom (dynamic) material.
    Uniform {
        /// The material whose uniform is driven.
        material: MaterialKey,
        /// Index into the material's `DynamicMaterial::values`.
        slot: usize,
    },
    /// A built-in PBR-family material factor (base color / metallic / etc).
    BuiltinParam {
        /// The material whose built-in factor is driven.
        material: MaterialKey,
        /// Which built-in factor to drive.
        param: BuiltinMaterialParam,
    },
    /// A punctual light's parameter.
    Light {
        /// The light to drive.
        light: LightKey,
        /// Which light parameter to drive.
        param: LightParam,
    },
    /// A camera's parameter.
    Camera {
        /// The camera to drive.
        camera: CameraKey,
        /// Which camera parameter to drive.
        param: CameraParam,
    },
}

/// Which scalar/color parameter of a [`crate::lights::Light`] an animation
/// channel drives. Params that don't apply to a given light variant are
/// silently ignored at apply time (e.g. `Range` on a directional light).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightParam {
    /// Light intensity (all variants).
    Intensity,
    /// Light color (all variants).
    Color,
    /// Falloff range (point / spot).
    Range,
    /// Spot inner cone angle (spot only).
    InnerAngle,
    /// Spot outer cone angle (spot only).
    OuterAngle,
}

/// Which built-in factor of a PBR-family material an animation channel drives.
/// Params a material kind lacks are silently ignored at apply time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinMaterialParam {
    /// Base color tint (rgb of `base_color_factor`).
    BaseColor,
    /// Metallic factor (PBR only).
    Metallic,
    /// Roughness factor (PBR only).
    Roughness,
    /// Emissive factor.
    Emissive,
}

/// Which parameter of a [`crate::cameras::CameraParams`] an animation channel
/// drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CameraParam {
    /// Vertical field-of-view (perspective only).
    FovY,
    /// Near clip plane.
    Near,
    /// Far clip plane.
    Far,
    /// Depth-of-field aperture.
    Aperture,
    /// Depth-of-field focus distance.
    FocusDistance,
}

/// One channel of a clip: a sampler that drives a single target.
#[derive(Debug, Clone)]
pub struct AnimationChannel {
    pub target: AnimationTarget,
    pub sampler: AnimationSampler,
}

impl AnimationChannel {
    pub fn new(target: AnimationTarget, sampler: AnimationSampler) -> Self {
        Self { target, sampler }
    }

    /// Sample this channel at an absolute clip-local time.
    pub fn sample(&self, time: f64) -> AnimationData {
        self.sampler.sample(time)
    }
}

/// A named group of channels sharing one clock.
#[derive(Debug, Clone)]
pub struct AnimationClipGroup {
    pub name: String,
    pub duration: f64,
    pub loop_style: Option<AnimationLoopStyle>,
    /// Playback-rate multiplier (dimensionless); see
    /// [`AnimationPlayer::speed`](super::player::AnimationPlayer::speed). The
    /// clock runs in seconds, so this carries no unit conversion.
    pub speed: f64,
    pub play_direction: AnimationPlayDirection,
    pub channels: Vec<AnimationChannel>,
    local_time: f64,
    state: AnimationState,
}

impl AnimationClipGroup {
    /// New group, defaulting to looping forward at the authored rate
    /// (`speed = 1.0`). The clock runs in **seconds** (the unit of `duration` and
    /// the channels' keyframe times).
    pub fn new(name: impl Into<String>, duration: f64, channels: Vec<AnimationChannel>) -> Self {
        Self {
            name: name.into(),
            duration,
            loop_style: Some(AnimationLoopStyle::Loop),
            speed: 1.0,
            play_direction: AnimationPlayDirection::Forward,
            channels,
            local_time: 0.0,
            state: AnimationState::Playing,
        }
    }

    /// The current shared local time (seconds).
    pub fn local_time(&self) -> f64 {
        self.local_time
    }

    /// Seek the shared clock (clamped to `[0, duration]`). Does not change state.
    pub fn set_local_time(&mut self, time: f64) {
        self.local_time = time.clamp(0.0, self.duration.max(0.0));
    }

    /// The current playback state.
    pub fn state(&self) -> &AnimationState {
        &self.state
    }

    /// Set the playback state (e.g. `Paused` while the editor scrubs).
    pub fn set_state(&mut self, state: AnimationState) {
        self.state = state;
    }

    /// Reset to the start, forward, playing.
    pub fn reset(&mut self) {
        self.local_time = 0.0;
        self.state = AnimationState::Playing;
        self.play_direction = AnimationPlayDirection::Forward;
    }

    /// Advance the **shared** clock by `dt_seconds`, wrapping per `loop_style`.
    /// Mirrors [`AnimationPlayer::update`](super::player::AnimationPlayer::update)
    /// exactly, but for the whole group at once (so every channel stays in sync).
    pub fn update(&mut self, dt_seconds: f64) {
        if self.state != AnimationState::Playing {
            return;
        }

        let local_time_delta = dt_seconds * self.speed;

        match self.play_direction {
            AnimationPlayDirection::Forward => {
                self.local_time += local_time_delta;
                if self.local_time >= self.duration {
                    match self.loop_style {
                        Some(AnimationLoopStyle::Loop) => {
                            self.local_time = self.local_time.rem_euclid(self.duration);
                        }
                        Some(AnimationLoopStyle::PingPong) => {
                            self.play_direction = AnimationPlayDirection::Backward;
                            self.local_time = self.duration;
                        }
                        None => {
                            self.local_time = self.duration;
                            self.state = AnimationState::Ended;
                        }
                    }
                }
            }
            AnimationPlayDirection::Backward => {
                self.local_time -= local_time_delta;
                if self.local_time <= 0.0 {
                    match self.loop_style {
                        Some(AnimationLoopStyle::Loop) => {
                            self.local_time =
                                self.duration - self.local_time.rem_euclid(self.duration);
                        }
                        Some(AnimationLoopStyle::PingPong) => {
                            self.play_direction = AnimationPlayDirection::Forward;
                            self.local_time = 0.0;
                        }
                        None => {
                            self.local_time = 0.0;
                            self.state = AnimationState::Ended;
                        }
                    }
                }
            }
        }
    }

    /// Sample every channel at the current shared `local_time`, calling `f` with
    /// each `(target, sampled value)`. Allocation-free — the mixer/update path
    /// uses this.
    pub fn for_each_sample(&self, mut f: impl FnMut(AnimationTarget, AnimationData)) {
        for channel in &self.channels {
            f(channel.target, channel.sample(self.local_time));
        }
    }

    /// Like [`Self::for_each_sample`] but at an arbitrary time (does not mutate the
    /// clock) — for one-shot pose reads / scrubbing.
    pub fn for_each_sample_at(&self, time: f64, mut f: impl FnMut(AnimationTarget, AnimationData)) {
        for channel in &self.channels {
            f(channel.target, channel.sample(time));
        }
    }

    /// Convenience: collect all `(target, value)` samples at the current
    /// `local_time` into a `Vec` (handy for tests).
    pub fn sample_all(&self) -> Vec<(AnimationTarget, AnimationData)> {
        let mut out = Vec::with_capacity(self.channels.len());
        self.for_each_sample(|t, v| out.push((t, v)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::{clip::AnimationClip, player::AnimationPlayer};
    use crate::transforms::TransformKey;

    fn f32_channel() -> AnimationChannel {
        // value goes 0 -> 10 linearly over t in [0, 1].
        AnimationChannel::new(
            AnimationTarget::Transform(TransformKey::default()),
            AnimationSampler::new_linear(
                vec![0.0, 1.0],
                vec![AnimationData::F32(0.0), AnimationData::F32(10.0)],
            ),
        )
    }

    fn group(loop_style: Option<AnimationLoopStyle>) -> AnimationClipGroup {
        let mut g = AnimationClipGroup::new("test", 1.0, vec![f32_channel()]);
        // speed defaults to 1.0 (seconds clock), which is what these tests drive.
        g.loop_style = loop_style;
        g
    }

    fn as_f32(d: &AnimationData) -> f32 {
        match d {
            AnimationData::F32(v) => *v,
            other => panic!("expected F32, got {other:?}"),
        }
    }

    #[test]
    fn clock_loop_wraps() {
        let mut g = group(Some(AnimationLoopStyle::Loop));
        g.update(0.5);
        assert!((g.local_time() - 0.5).abs() < 1e-9);
        g.update(0.6); // 1.1 -> wraps to 0.1
        assert!(
            (g.local_time() - 0.1).abs() < 1e-9,
            "got {}",
            g.local_time()
        );
        assert_eq!(g.state(), &AnimationState::Playing);
    }

    #[test]
    fn clock_pingpong_reverses() {
        let mut g = group(Some(AnimationLoopStyle::PingPong));
        g.update(1.2); // past the end -> clamp to duration, flip to Backward
        assert!((g.local_time() - 1.0).abs() < 1e-9);
        assert_eq!(g.play_direction, AnimationPlayDirection::Backward);
        g.update(0.3); // now decreasing
        assert!(
            (g.local_time() - 0.7).abs() < 1e-9,
            "got {}",
            g.local_time()
        );
    }

    #[test]
    fn clock_once_ends_and_clamps() {
        let mut g = group(None);
        g.update(1.5);
        assert!((g.local_time() - 1.0).abs() < 1e-9);
        assert_eq!(g.state(), &AnimationState::Ended);
        // further updates are no-ops once ended
        g.update(1.0);
        assert!((g.local_time() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn seek_clamps() {
        let mut g = group(Some(AnimationLoopStyle::Loop));
        g.set_local_time(2.0);
        assert!((g.local_time() - 1.0).abs() < 1e-9);
        g.set_local_time(-1.0);
        assert!(g.local_time().abs() < 1e-9);
    }

    #[test]
    fn sample_all_interpolates() {
        let mut g = group(Some(AnimationLoopStyle::Loop));
        g.set_local_time(0.5);
        let samples = g.sample_all();
        assert_eq!(samples.len(), 1);
        assert!((as_f32(&samples[0].1) - 5.0).abs() < 1e-6);
    }

    /// Guard: the group's shared clock must advance identically to a loose
    /// `AnimationPlayer` for the same inputs (so a single-clip group reduces to
    /// the loose-player behavior, byte-for-byte).
    #[test]
    fn clock_matches_player() {
        let clip = AnimationClip::new(
            Some("p".to_string()),
            1.0,
            AnimationSampler::new_linear(
                vec![0.0, 1.0],
                vec![AnimationData::F32(0.0), AnimationData::F32(10.0)],
            ),
        );
        let mut player = AnimationPlayer::new(clip);
        let mut g = group(Some(AnimationLoopStyle::Loop));

        for &dt in &[0.3, 0.3, 0.3, 0.3, 0.3, 0.7, 0.9] {
            player.update(dt);
            g.update(dt);
            assert!(
                (player.local_time() - g.local_time()).abs() < 1e-9,
                "player {} != group {}",
                player.local_time(),
                g.local_time()
            );
        }
    }
}
