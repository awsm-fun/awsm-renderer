//! Pure-CPU curve math. See [`README.md`](../README.md).

pub mod curve1;
pub mod curve3;
pub mod helpers;

pub use curve1::{Curve1, ConstCurve1, LinearCurve1, KeyedCurve1, Curve1Key};
pub use curve3::{Curve3, BezierCurve, CatmullRomCurve, Frame, FrameSequence};
pub use helpers::{nearest_point_on_curve, curve_length_between};
