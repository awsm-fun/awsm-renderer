//! Curve-aware geometry helpers. Live here, not in `awsm-geometry`, because their
//! primary input is a curve.

use glam::Vec3;

use crate::curve3::Curve3;

/// Find the closest point on a curve to query point `p`, returning `(t, distance)`.
///
/// Uses a two-stage strategy: coarse sampling to find the best segment, then a fixed
/// number of golden-section iterations within that segment for refinement.
pub fn nearest_point_on_curve<C: Curve3 + ?Sized>(
    curve: &C,
    p: Vec3,
    coarse_samples: usize,
) -> (f32, f32) {
    let n = coarse_samples.max(8);
    let mut best_t = 0.0_f32;
    let mut best_d2 = f32::MAX;
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let q = curve.point_at(t);
        let d2 = (q - p).length_squared();
        if d2 < best_d2 {
            best_d2 = d2;
            best_t = t;
        }
    }
    // Golden-section refinement in a window around best_t.
    let half_window = 1.0 / n as f32;
    let mut lo = (best_t - half_window).max(0.0);
    let mut hi = (best_t + half_window).min(1.0);
    let phi = 0.618_034_f32;
    for _ in 0..16 {
        let a = hi - (hi - lo) * phi;
        let b = lo + (hi - lo) * phi;
        let pa = curve.point_at(a);
        let pb = curve.point_at(b);
        let da = (pa - p).length_squared();
        let db = (pb - p).length_squared();
        if da < db {
            hi = b;
        } else {
            lo = a;
        }
    }
    let t = (lo + hi) * 0.5;
    let q = curve.point_at(t);
    let dist = (q - p).length();
    (t, dist)
}

/// Approximate arc length between two parameter values along a curve.
///
/// Uses `subdivisions` chord-sums; raise this for higher accuracy. Returns 0.0 if
/// `b <= a`.
pub fn curve_length_between<C: Curve3 + ?Sized>(
    curve: &C,
    a: f32,
    b: f32,
    subdivisions: usize,
) -> f32 {
    let a = a.clamp(0.0, 1.0);
    let b = b.clamp(0.0, 1.0);
    if b <= a {
        return 0.0;
    }
    let n = subdivisions.max(2);
    let span = b - a;
    let mut prev = curve.point_at(a);
    let mut sum = 0.0_f32;
    for i in 1..=n {
        let t = a + span * (i as f32 / n as f32);
        let p = curve.point_at(t);
        sum += (p - prev).length();
        prev = p;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve3::CatmullRomCurve;

    #[test]
    fn nearest_on_straight_segment() {
        let curve = CatmullRomCurve::new(vec![Vec3::ZERO, Vec3::X * 10.0], false);
        let (_t, d) = nearest_point_on_curve(&curve, Vec3::new(5.0, 1.0, 0.0), 16);
        // Distance from (5,1,0) to nearest point on line should be ~1.
        assert!((d - 1.0).abs() < 0.05);
    }

    #[test]
    fn curve_length_full_range_matches_total() {
        let curve = CatmullRomCurve::new(vec![Vec3::ZERO, Vec3::X * 10.0], false);
        let l = curve_length_between(&curve, 0.0, 1.0, 64);
        assert!((l - 10.0).abs() < 0.1);
    }
}
