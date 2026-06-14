//! 1D parameter curves over `[0, 1]` with output type `T`.
//!
//! Used by particles (`color_over_life`, `size_over_life`, `alpha_over_life`) and any
//! "value over normalized parameter" need. Not a timeline — animation curves over real
//! time are a separate concern.

use glam::Vec3;

/// 1D parameter curve over `[0, 1]` producing values of type `T`.
pub trait Curve1<T: Clone> {
    fn sample(&self, t: f32) -> T;
}

/// Constant value across the entire parameter range.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ConstCurve1<T: Clone> {
    pub value: T,
}

impl<T: Clone> Curve1<T> for ConstCurve1<T> {
    fn sample(&self, _t: f32) -> T {
        self.value.clone()
    }
}

/// Linear interpolation between `start` and `end` over `[0, 1]`.
pub trait Lerp1 {
    fn lerp1(a: &Self, b: &Self, t: f32) -> Self;
}

impl Lerp1 for f32 {
    fn lerp1(a: &Self, b: &Self, t: f32) -> Self {
        a + (b - a) * t
    }
}

impl Lerp1 for Vec3 {
    fn lerp1(a: &Self, b: &Self, t: f32) -> Self {
        a.lerp(*b, t)
    }
}

impl Lerp1 for [f32; 4] {
    fn lerp1(a: &Self, b: &Self, t: f32) -> Self {
        [
            f32::lerp1(&a[0], &b[0], t),
            f32::lerp1(&a[1], &b[1], t),
            f32::lerp1(&a[2], &b[2], t),
            f32::lerp1(&a[3], &b[3], t),
        ]
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LinearCurve1<T: Clone> {
    pub start: T,
    pub end: T,
}

impl<T: Clone + Lerp1> Curve1<T> for LinearCurve1<T> {
    fn sample(&self, t: f32) -> T {
        let t = t.clamp(0.0, 1.0);
        T::lerp1(&self.start, &self.end, t)
    }
}

/// Piecewise-linear curve through arbitrary keys.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KeyedCurve1<T: Clone> {
    /// (t, value) pairs sorted by `t \in [0, 1]`.
    pub keys: Vec<Curve1Key<T>>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Curve1Key<T: Clone> {
    pub t: f32,
    pub value: T,
}

impl<T: Clone + Lerp1> Curve1<T> for KeyedCurve1<T> {
    fn sample(&self, t: f32) -> T {
        if self.keys.is_empty() {
            panic!("KeyedCurve1 has no keys");
        }
        if self.keys.len() == 1 || t <= self.keys[0].t {
            return self.keys[0].value.clone();
        }
        let last = &self.keys[self.keys.len() - 1];
        if t >= last.t {
            return last.value.clone();
        }
        for window in self.keys.windows(2) {
            let a = &window[0];
            let b = &window[1];
            if t >= a.t && t <= b.t {
                let span = (b.t - a.t).max(1.0e-6);
                let local = (t - a.t) / span;
                return T::lerp1(&a.value, &b.value, local);
            }
        }
        last.value.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_curve1_returns_value() {
        let c = ConstCurve1 { value: 0.5_f32 };
        assert_eq!(c.sample(0.0), 0.5);
        assert_eq!(c.sample(1.0), 0.5);
    }

    #[test]
    fn linear_curve1_f32() {
        let c = LinearCurve1 {
            start: 0.0_f32,
            end: 10.0,
        };
        assert!((c.sample(0.0) - 0.0).abs() < 1.0e-6);
        assert!((c.sample(0.5) - 5.0).abs() < 1.0e-6);
        assert!((c.sample(1.0) - 10.0).abs() < 1.0e-6);
    }

    #[test]
    fn keyed_curve1_through_keys() {
        let c = KeyedCurve1 {
            keys: vec![
                Curve1Key {
                    t: 0.0,
                    value: 0.0_f32,
                },
                Curve1Key {
                    t: 0.5,
                    value: 10.0,
                },
                Curve1Key { t: 1.0, value: 0.0 },
            ],
        };
        assert!((c.sample(0.0) - 0.0).abs() < 1.0e-6);
        assert!((c.sample(0.25) - 5.0).abs() < 1.0e-6);
        assert!((c.sample(0.5) - 10.0).abs() < 1.0e-6);
        assert!((c.sample(0.75) - 5.0).abs() < 1.0e-6);
        assert!((c.sample(1.0) - 0.0).abs() < 1.0e-6);
    }

    #[test]
    fn linear_curve1_clamps_out_of_range() {
        // `t` outside [0,1] saturates to the endpoints (particle "over life"
        // sampling can be handed a slightly-out-of-range t at the boundaries).
        let c = LinearCurve1 {
            start: 2.0_f32,
            end: 8.0,
        };
        assert!((c.sample(-1.0) - 2.0).abs() < 1.0e-6, "t<0 → start");
        assert!((c.sample(5.0) - 8.0).abs() < 1.0e-6, "t>1 → end");
    }

    #[test]
    fn keyed_curve1_clamps_to_endpoints_and_handles_single_key() {
        let c = KeyedCurve1 {
            keys: vec![
                Curve1Key {
                    t: 0.2,
                    value: 3.0_f32,
                },
                Curve1Key { t: 0.8, value: 9.0 },
            ],
        };
        // Before the first / after the last key → that endpoint's value (no
        // extrapolation past the key range).
        assert!(
            (c.sample(0.0) - 3.0).abs() < 1.0e-6,
            "t before first key → first value"
        );
        assert!(
            (c.sample(1.0) - 9.0).abs() < 1.0e-6,
            "t after last key → last value"
        );
        // Midway between the two keys (t=0.5 of [0.2,0.8]) → halfway value.
        assert!((c.sample(0.5) - 6.0).abs() < 1.0e-6);

        // A single-key curve is constant at that key's value for any t.
        let one = KeyedCurve1 {
            keys: vec![Curve1Key {
                t: 0.5,
                value: 7.0_f32,
            }],
        };
        assert_eq!(one.sample(0.0), 7.0);
        assert_eq!(one.sample(1.0), 7.0);
    }

    #[test]
    fn lerp1_vec3_and_vec4_components() {
        // The Vec3 / [f32;4] Lerp1 impls (only f32 was covered above).
        let v = LinearCurve1 {
            start: Vec3::new(0.0, 2.0, 4.0),
            end: Vec3::new(10.0, 6.0, 0.0),
        };
        assert_eq!(v.sample(0.5), Vec3::new(5.0, 4.0, 2.0));

        let c = LinearCurve1 {
            start: [0.0_f32, 0.0, 0.0, 1.0],
            end: [1.0, 0.5, 0.25, 0.0],
        };
        let mid = c.sample(0.5);
        assert!((mid[0] - 0.5).abs() < 1e-6);
        assert!((mid[1] - 0.25).abs() < 1e-6);
        assert!((mid[2] - 0.125).abs() < 1e-6);
        assert!((mid[3] - 0.5).abs() < 1e-6);
    }
}
