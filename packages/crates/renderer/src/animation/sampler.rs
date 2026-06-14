//! Animation sampling and interpolation.

use std::cmp::Ordering;

use super::{data::AnimationData, Animatable};

/// Keyframe sampler for animation data.
#[derive(Debug, Clone)]
pub enum AnimationSampler<T = AnimationData> {
    Linear {
        times: Vec<f64>,
        values: Vec<T>,
    },
    Step {
        times: Vec<f64>,
        values: Vec<T>,
    },
    CubicSpline {
        times: Vec<f64>,
        values: Vec<T>,
        in_tangents: Vec<T>,
        out_tangents: Vec<T>,
    },
}

impl<T: Animatable> AnimationSampler<T> {
    /// Creates a linear interpolation sampler.
    pub fn new_linear(times: Vec<f64>, values: Vec<T>) -> Self {
        Self::Linear { times, values }
    }

    /// Creates a step interpolation sampler.
    pub fn new_step(times: Vec<f64>, values: Vec<T>) -> Self {
        Self::Step { times, values }
    }

    /// Creates a cubic spline interpolation sampler.
    pub fn new_cubic_spline(
        times: Vec<f64>,
        values: Vec<T>,
        in_tangents: Vec<T>,
        out_tangents: Vec<T>,
    ) -> Self {
        Self::CubicSpline {
            times,
            values,
            in_tangents,
            out_tangents,
        }
    }

    /// Returns the keyframe times for this sampler.
    pub fn times(&self) -> &[f64] {
        match self {
            Self::Linear { times, .. } => times,
            Self::Step { times, .. } => times,
            Self::CubicSpline { times, .. } => times,
        }
    }

    /// Samples the animation at the given time.
    pub fn sample(&self, time: f64) -> T {
        let bounds = self.binary_search_bounds(time);

        match bounds {
            BinaryBounds::ExactHit(index) => match self {
                AnimationSampler::Linear { values, .. } => values[index].clone(),
                AnimationSampler::Step { values, .. } => values[index].clone(),
                AnimationSampler::CubicSpline { values, .. } => values[index].clone(),
            },
            BinaryBounds::Between(left_index, right_index) => {
                let times = self.times();
                let left_time = times[left_index];
                let right_time = times[right_index];

                match self {
                    AnimationSampler::Linear { values, .. } => {
                        let left_value = &values[left_index];
                        let right_value = &values[right_index];

                        let interpolation_time = (time - left_time) / (right_time - left_time);

                        T::interpolate_linear(left_value, right_value, interpolation_time)
                    }
                    AnimationSampler::Step { values, .. } => values[left_index].clone(),
                    AnimationSampler::CubicSpline {
                        values,
                        in_tangents,
                        out_tangents,
                        ..
                    } => {
                        let interpolation_time = (time - left_time) / (right_time - left_time);
                        let delta_time = right_time - left_time;
                        let left_value = &values[left_index];
                        let right_value = &values[right_index];
                        let left_tangent = &out_tangents[left_index];
                        let right_tangent = &in_tangents[right_index];

                        T::interpolate_cubic_spline(
                            left_value,
                            left_tangent,
                            right_value,
                            right_tangent,
                            delta_time,
                            interpolation_time,
                        )
                    }
                }
            }
        }
    }

    // Returns the index of the keyframe that is closest to the given time
    // BinaryBounds::ExactHit(usize) if the time is exactly on a keyframe
    // BinaryBounds::Middle(usize, usize) if the time is between two keyframes
    fn binary_search_bounds(&self, time: f64) -> BinaryBounds {
        let times = self.times();

        if times.is_empty() {
            panic!("Cannot search an empty times array");
        }

        match times.binary_search_by(|t| t.partial_cmp(&time).unwrap_or(Ordering::Equal)) {
            Ok(i) => BinaryBounds::ExactHit(i),
            Err(i) => {
                if i == 0 {
                    // `time` is before the first keyframe. glTF holds sampler
                    // output constant outside the keyframe range, so CLAMP to the
                    // first keyframe (mirrors the `i >= len` after-last clamp
                    // below). The old `Between(0, 1)` extrapolated a negative
                    // interpolation factor below the first value — wrong for a
                    // track whose first key starts after the clip's t=0 — and
                    // panicked (`times[1]` OOB) for a single-keyframe track.
                    BinaryBounds::ExactHit(0)
                } else if i >= times.len() {
                    // `time` is after the last keyframe — clamp to the end.
                    BinaryBounds::ExactHit(times.len() - 1)
                } else {
                    BinaryBounds::Between(i - 1, i)
                }
            }
        }
    }
}

enum BinaryBounds {
    ExactHit(usize),
    Between(usize, usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::Animatable;

    // Scalar test type: sampling is index/clamp logic independent of the value
    // type, so a plain f64 keeps the keyframe-selection assertions readable.
    impl Animatable for f64 {
        fn interpolate_linear(first: &Self, second: &Self, t: f64) -> Self {
            first + (second - first) * t
        }
        fn interpolate_cubic_spline(
            first_value: &Self,
            _first_tangent: &Self,
            second_value: &Self,
            _second_tangent: &Self,
            _delta_time: f64,
            t: f64,
        ) -> Self {
            // Tangents irrelevant for these index-selection tests.
            first_value + (second_value - first_value) * t
        }
    }

    #[test]
    fn linear_interpolates_between_and_clamps_outside() {
        let s = AnimationSampler::new_linear(vec![1.0, 2.0, 3.0], vec![10.0, 20.0, 30.0]);
        // Exact hits.
        assert_eq!(s.sample(1.0), 10.0);
        assert_eq!(s.sample(2.0), 20.0);
        assert_eq!(s.sample(3.0), 30.0);
        // Between.
        assert_eq!(s.sample(1.5), 15.0);
        assert_eq!(s.sample(2.25), 22.5);
        // BEFORE first → clamp to first (was extrapolated to a negative factor).
        assert_eq!(s.sample(0.0), 10.0);
        assert_eq!(s.sample(-100.0), 10.0);
        // AFTER last → clamp to last.
        assert_eq!(s.sample(3.0001), 30.0);
        assert_eq!(s.sample(99.0), 30.0);
    }

    #[test]
    fn step_holds_left_and_clamps() {
        let s = AnimationSampler::new_step(vec![1.0, 2.0], vec![10.0, 20.0]);
        assert_eq!(s.sample(1.4), 10.0); // holds left value
        assert_eq!(s.sample(0.0), 10.0); // before first → first
        assert_eq!(s.sample(9.0), 20.0); // after last → last
    }

    #[test]
    fn single_keyframe_never_panics_and_holds() {
        // A constant (one-keyframe) track: sampling before/at/after its time must
        // return that value with NO out-of-bounds panic (the old before-first
        // `Between(0, 1)` indexed `times[1]`).
        for s in [
            AnimationSampler::new_linear(vec![0.5], vec![42.0]),
            AnimationSampler::new_step(vec![0.5], vec![42.0]),
        ] {
            assert_eq!(s.sample(0.0), 42.0); // before the only key
            assert_eq!(s.sample(0.5), 42.0); // exact
            assert_eq!(s.sample(5.0), 42.0); // after
        }
    }

    #[test]
    fn cubic_spline_endpoints_clamp() {
        let s = AnimationSampler::new_cubic_spline(
            vec![1.0, 2.0],
            vec![10.0, 20.0],
            vec![0.0, 0.0],
            vec![0.0, 0.0],
        );
        assert_eq!(s.sample(1.0), 10.0);
        assert_eq!(s.sample(2.0), 20.0);
        assert_eq!(s.sample(0.0), 10.0); // before first → clamp
        assert_eq!(s.sample(9.0), 20.0); // after last → clamp
    }
}
