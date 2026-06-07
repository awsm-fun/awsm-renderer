//! Animation playback state and controls.

use super::{clip::AnimationClip, data::AnimationData};

/// Animation player for a clip.
#[derive(Debug, Clone)]
pub struct AnimationPlayer<T = AnimationData> {
    /// Playback-rate multiplier (dimensionless): `1.0` plays at the authored
    /// rate, `2.0` double speed, `0.5` half. The clock runs in **seconds**
    /// throughout — `update` is handed a seconds delta — so this carries no unit
    /// conversion (`update_animations` converts its millisecond input once).
    pub speed: f64,
    pub loop_style: Option<AnimationLoopStyle>,
    // will change with ping-pong as each end is hit
    pub play_direction: AnimationPlayDirection,
    clip: AnimationClip<T>,
    state: AnimationState,
    local_time: f64,
}

/// Playback state for an animation player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimationState {
    Playing,
    Paused,
    Ended,
}

/// Looping behavior for animation playback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimationLoopStyle {
    Loop,
    PingPong,
}

/// Playback direction for an animation player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimationPlayDirection {
    Forward,
    Backward,
}

impl<T> AnimationPlayer<T> {
    /// Creates a new animation player for a clip.
    pub fn new(clip: AnimationClip<T>) -> Self {
        Self {
            speed: 1.0,
            loop_style: Some(AnimationLoopStyle::Loop),
            play_direction: AnimationPlayDirection::Forward,
            clip,
            state: AnimationState::Playing,
            local_time: 0.0,
        }
    }

    /// Advances the animation by `global_time_delta` **seconds** (the same unit
    /// as the clip's keyframe times / duration; `update_animations` converts the
    /// frame's millisecond delta to seconds before calling this).
    pub fn update(&mut self, global_time_delta: f64) {
        if self.state != AnimationState::Playing {
            return;
        }

        let local_time_delta = global_time_delta * self.speed;

        match self.play_direction {
            AnimationPlayDirection::Forward => {
                self.local_time += local_time_delta;
                if self.local_time >= self.clip.duration {
                    match self.loop_style {
                        Some(AnimationLoopStyle::Loop) => {
                            self.local_time = self.local_time.rem_euclid(self.clip.duration);
                        }
                        Some(AnimationLoopStyle::PingPong) => {
                            self.play_direction = AnimationPlayDirection::Backward;
                            self.local_time = self.clip.duration;
                        }
                        None => {
                            self.local_time = self.clip.duration;
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
                                self.clip.duration - self.local_time.rem_euclid(self.clip.duration);
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
}

impl<T> AnimationPlayer<T> {
    /// The clip's duration (seconds).
    pub fn duration(&self) -> f64 {
        self.clip.duration
    }

    /// The current local playback time (seconds).
    pub fn local_time(&self) -> f64 {
        self.local_time
    }

    /// Seeks to an absolute local time (clamped to `[0, duration]`). Used by the
    /// editor transport to scrub a paused clip. Does not change `state`.
    pub fn set_local_time(&mut self, time: f64) {
        self.local_time = time.clamp(0.0, self.clip.duration.max(0.0));
    }

    /// The current playback state.
    pub fn state(&self) -> &AnimationState {
        &self.state
    }

    /// Sets the playback state (e.g. `Paused` while the editor scrubs).
    pub fn set_state(&mut self, state: AnimationState) {
        self.state = state;
    }

    /// Resets to the start, forward, playing.
    pub fn reset(&mut self) {
        self.local_time = 0.0;
        self.state = AnimationState::Playing;
        self.play_direction = AnimationPlayDirection::Forward;
    }
}

impl AnimationPlayer<AnimationData> {
    /// Samples the animation at the current local time.
    pub fn sample(&self) -> AnimationData {
        self.clip.sampler.sample(self.local_time)
    }

    /// Samples at an arbitrary time without mutating `local_time` (for the editor
    /// scrubbing a paused clip / one-shot pose reads).
    pub fn sample_at(&self, time: f64) -> AnimationData {
        self.clip.sampler.sample(time)
    }
}
