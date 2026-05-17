//! Pure-CPU curve math. See [`README.md`](../README.md).

pub mod curve1;
pub mod curve3;
pub mod helpers;

pub use curve1::{ConstCurve1, Curve1, Curve1Key, KeyedCurve1, LinearCurve1};
pub use curve3::{BezierCurve, CatmullRomCurve, Curve3, Frame, FrameSequence};
pub use helpers::{curve_length_between, nearest_point_on_curve};
