//! Animation playback state and controls.

use super::{clip::AnimationClip, data::AnimationData};

/// Animation player for a clip.
#[derive(Debug, Clone)]
pub struct AnimationPlayer<T = AnimationData> {
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
            speed: 1.0 / 1000.0,
            loop_style: Some(AnimationLoopStyle::Loop),
            play_direction: AnimationPlayDirection::Forward,
            clip,
            state: AnimationState::Playing,
            local_time: 0.0,
        }
    }

    /// Advances the animation by the given global time delta.
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

impl AnimationPlayer<AnimationData> {
    /// Samples the animation at the current local time.
    pub fn sample(&self) -> AnimationData {
        self.clip.sampler.sample(self.local_time)
    }
}
