//! 3D path curves: trait + Catmull-Rom + Bezier impls + tangent-frame helpers.

use glam::{Quat, Vec3};

/// 3D path curve sampled in normalized parameter `t \in [0, 1]`.
pub trait Curve3 {
    /// Position at `t`.
    fn point_at(&self, t: f32) -> Vec3;

    /// Unit tangent at `t`. Default impl uses central difference.
    fn tangent_at(&self, t: f32) -> Vec3 {
        let eps = 1.0e-4_f32;
        let t_minus = (t - eps).max(0.0);
        let t_plus = (t + eps).min(1.0);
        let dt = t_plus - t_minus;
        if dt <= 0.0 {
            Vec3::Z
        } else {
            let a = self.point_at(t_minus);
            let b = self.point_at(t_plus);
            (b - a).normalize_or_zero()
        }
    }

    /// Returns `n` evenly-spaced sample points (in parameter, not arc-length).
    fn get_spaced_points(&self, n: usize) -> Vec<Vec3> {
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            return vec![self.point_at(0.0)];
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f32 / (n - 1) as f32;
            out.push(self.point_at(t));
        }
        out
    }

    /// Approximate arc length, by sampling at `n` subdivisions and summing chord lengths.
    fn total_length(&self, n: usize) -> f32 {
        let n = n.max(2);
        let mut prev = self.point_at(0.0);
        let mut sum = 0.0_f32;
        for i in 1..n {
            let t = i as f32 / (n - 1) as f32;
            let p = self.point_at(t);
            sum += (p - prev).length();
            prev = p;
        }
        sum
    }

    /// Whether this curve is closed (the last knot wraps back to the first).
    fn is_closed(&self) -> bool {
        false
    }
}

/// A position + orthonormal basis along a curve.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub position: Vec3,
    pub tangent: Vec3,
    pub normal: Vec3,
    pub binormal: Vec3,
}

impl Frame {
    /// Returns the rotation that maps Z+ to the tangent and Y+ to the normal.
    pub fn rotation(&self) -> Quat {
        let mat = glam::Mat3::from_cols(self.binormal, self.normal, self.tangent);
        Quat::from_mat3(&mat)
    }
}

/// Sequence of frames along a curve, computed by parallel-transport for "up" stability.
#[derive(Debug, Clone)]
pub struct FrameSequence {
    pub frames: Vec<Frame>,
}

impl FrameSequence {
    /// Build a sequence of `n` frames along the curve using parallel-transport from an
    /// initial up vector. Stable through full 360° turns; avoids the flips that
    /// strict Frenet frames produce at inflection points.
    pub fn parallel_transport<C: Curve3 + ?Sized>(curve: &C, n: usize, initial_up: Vec3) -> Self {
        let n = n.max(2);
        let positions = curve.get_spaced_points(n);
        let mut frames: Vec<Frame> = Vec::with_capacity(n);

        let mut prev_tangent = curve.tangent_at(0.0);
        if prev_tangent.length_squared() < 1.0e-12 {
            prev_tangent = Vec3::Z;
        }
        let mut prev_normal = {
            let proj = initial_up - prev_tangent * initial_up.dot(prev_tangent);
            let n = proj.normalize_or_zero();
            if n.length_squared() < 1.0e-12 {
                if prev_tangent.dot(Vec3::Y).abs() < 0.99 {
                    (Vec3::Y - prev_tangent * prev_tangent.dot(Vec3::Y)).normalize()
                } else {
                    Vec3::X
                }
            } else {
                n
            }
        };
        let mut prev_binormal = prev_tangent.cross(prev_normal).normalize_or_zero();

        for (i, &position) in positions.iter().enumerate() {
            let t = i as f32 / (n - 1) as f32;
            let tangent = {
                let t = curve.tangent_at(t);
                if t.length_squared() < 1.0e-12 {
                    prev_tangent
                } else {
                    t
                }
            };
            // Rotate prev_normal from prev_tangent to tangent by the minimum rotation.
            let dot = prev_tangent.dot(tangent).clamp(-1.0, 1.0);
            let normal = if dot >= 0.9999 {
                prev_normal
            } else {
                let axis = prev_tangent.cross(tangent).normalize_or_zero();
                if axis.length_squared() < 1.0e-12 {
                    prev_normal
                } else {
                    let angle = dot.acos();
                    let q = Quat::from_axis_angle(axis, angle);
                    q.mul_vec3(prev_normal).normalize_or_zero()
                }
            };
            let binormal = tangent.cross(normal).normalize_or_zero();

            frames.push(Frame {
                position,
                tangent,
                normal: if normal.length_squared() > 0.0 {
                    normal
                } else {
                    prev_normal
                },
                binormal: if binormal.length_squared() > 0.0 {
                    binormal
                } else {
                    prev_binormal
                },
            });
            prev_tangent = tangent;
            prev_normal = normal;
            prev_binormal = binormal;
        }

        Self { frames }
    }
}

/// Catmull-Rom spline through a set of control points.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CatmullRomCurve {
    pub points: Vec<Vec3>,
    pub closed: bool,
    /// Tension parameter (0.5 = classic Catmull-Rom).
    pub tension: f32,
}

impl CatmullRomCurve {
    pub fn new(points: Vec<Vec3>, closed: bool) -> Self {
        Self {
            points,
            closed,
            tension: 0.5,
        }
    }

    fn segment_count(&self) -> usize {
        if self.points.len() < 2 {
            return 0;
        }
        if self.closed {
            self.points.len()
        } else {
            self.points.len() - 1
        }
    }

    fn knot(&self, idx: isize) -> Vec3 {
        let len = self.points.len() as isize;
        if len == 0 {
            return Vec3::ZERO;
        }
        if self.closed {
            let wrapped = idx.rem_euclid(len) as usize;
            self.points[wrapped]
        } else {
            let clamped = idx.clamp(0, len - 1) as usize;
            self.points[clamped]
        }
    }
}

impl Curve3 for CatmullRomCurve {
    fn point_at(&self, t: f32) -> Vec3 {
        let seg_count = self.segment_count();
        if seg_count == 0 {
            return self.points.first().copied().unwrap_or(Vec3::ZERO);
        }
        let t = t.clamp(0.0, 1.0);
        let scaled = t * seg_count as f32;
        let mut seg_idx = scaled.floor() as isize;
        if seg_idx >= seg_count as isize {
            seg_idx = seg_count as isize - 1;
        }
        let local = scaled - seg_idx as f32;

        let p0 = self.knot(seg_idx - 1);
        let p1 = self.knot(seg_idx);
        let p2 = self.knot(seg_idx + 1);
        let p3 = self.knot(seg_idx + 2);

        let s = self.tension;
        let t1 = local;
        let t2 = t1 * t1;
        let t3 = t2 * t1;

        let m1 = (p2 - p0) * s;
        let m2 = (p3 - p1) * s;

        let a0 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let a1 = t3 - 2.0 * t2 + t1;
        let a2 = -2.0 * t3 + 3.0 * t2;
        let a3 = t3 - t2;

        p1 * a0 + m1 * a1 + p2 * a2 + m2 * a3
    }

    fn is_closed(&self) -> bool {
        self.closed
    }
}

/// Composite cubic Bezier curve through a sequence of control points
/// (control points: [p0, c0, c1, p1, c2, c3, p2, ...]; segments share end points).
///
/// When `closed` is true the curve wraps the last anchor back to the
/// first as an additional segment, using the optional wrap-handle pair
/// at `handles[2*(N-1) .. 2*N]` (out-handle of anchor N-1 then
/// in-handle of anchor 0). Missing wrap handles fall back to the
/// anchor positions themselves, which degrades the wrap into a
/// straight chord — sensible visual placeholder but the caller should
/// provide handles for a smooth loop.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BezierCurve {
    /// Sequence of anchor points (length N).
    pub anchors: Vec<Vec3>,
    /// Two control handles per segment — out-handle of anchor i then
    /// in-handle of anchor i+1. Length `2 * (N - 1)` for an open curve;
    /// `2 * N` for a closed curve (the trailing pair bridges anchor
    /// N-1 back to anchor 0).
    pub handles: Vec<Vec3>,
    pub closed: bool,
}

impl BezierCurve {
    fn segment_count(&self) -> usize {
        if self.anchors.len() < 2 {
            return 0;
        }
        if self.closed {
            self.anchors.len()
        } else {
            self.anchors.len() - 1
        }
    }
}

impl Curve3 for BezierCurve {
    fn point_at(&self, t: f32) -> Vec3 {
        let seg_count = self.segment_count();
        if seg_count == 0 {
            return self.anchors.first().copied().unwrap_or(Vec3::ZERO);
        }
        let t = t.clamp(0.0, 1.0);
        let scaled = t * seg_count as f32;
        let mut seg_idx = scaled.floor() as usize;
        if seg_idx >= seg_count {
            seg_idx = seg_count - 1;
        }
        let u = scaled - seg_idx as f32;

        let p0 = self.anchors[seg_idx];
        // For the wrap segment of a closed curve, the next anchor is
        // anchor 0. `% anchors.len()` keeps the open case unchanged
        // (seg_idx + 1 < N for every non-wrap segment).
        let p3 = self.anchors[(seg_idx + 1) % self.anchors.len()];
        let handle_base = seg_idx * 2;
        let p1 = self.handles.get(handle_base).copied().unwrap_or(p0);
        let p2 = self.handles.get(handle_base + 1).copied().unwrap_or(p3);

        let one_minus_u = 1.0 - u;
        let b0 = one_minus_u * one_minus_u * one_minus_u;
        let b1 = 3.0 * one_minus_u * one_minus_u * u;
        let b2 = 3.0 * one_minus_u * u * u;
        let b3 = u * u * u;

        p0 * b0 + p1 * b1 + p2 * b2 + p3 * b3
    }

    fn is_closed(&self) -> bool {
        self.closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catmull_rom_passes_through_knots() {
        let pts = vec![Vec3::ZERO, Vec3::X, Vec3::X * 2.0, Vec3::X * 3.0];
        let curve = CatmullRomCurve::new(pts.clone(), false);
        // At t=0 we should be at the first knot.
        let p0 = curve.point_at(0.0);
        assert!((p0 - pts[0]).length() < 1.0e-5);
        // At t=1 we should be at the last knot.
        let p1 = curve.point_at(1.0);
        assert!((p1 - pts[pts.len() - 1]).length() < 1.0e-5);
    }

    #[test]
    fn bezier_endpoints_at_anchors() {
        let curve = BezierCurve {
            anchors: vec![Vec3::ZERO, Vec3::new(3.0, 0.0, 0.0)],
            handles: vec![Vec3::new(1.0, 1.0, 0.0), Vec3::new(2.0, 1.0, 0.0)],
            closed: false,
        };
        assert!(curve.point_at(0.0).abs_diff_eq(Vec3::ZERO, 1.0e-5));
        assert!(curve
            .point_at(1.0)
            .abs_diff_eq(Vec3::new(3.0, 0.0, 0.0), 1.0e-5));
    }

    #[test]
    fn bezier_closed_wraps_to_first_anchor() {
        // Triangle of anchors with handles set to anchor positions —
        // the loop degenerates into straight chords, which is enough
        // to test that the wrap segment exists at all (segment_count
        // returns N, t=1 lands back on anchors[0]).
        let a0 = Vec3::ZERO;
        let a1 = Vec3::new(1.0, 0.0, 0.0);
        let a2 = Vec3::new(0.5, 1.0, 0.0);
        let curve = BezierCurve {
            anchors: vec![a0, a1, a2],
            handles: vec![a0, a1, a1, a2, a2, a0], // 3 segments × 2 handles
            closed: true,
        };

        // t=1 must close the loop back to the first anchor.
        assert!(
            curve.point_at(1.0).abs_diff_eq(a0, 1.0e-5),
            "closed curve t=1 should land on anchors[0], got {:?}",
            curve.point_at(1.0),
        );

        // Sample the open version of the same anchors — t=1 lands on
        // the last anchor instead, so the closed/open behavior is
        // distinguishable.
        let open = BezierCurve {
            anchors: vec![a0, a1, a2],
            handles: vec![a0, a1, a1, a2],
            closed: false,
        };
        assert!(open.point_at(1.0).abs_diff_eq(a2, 1.0e-5));
    }

    #[test]
    fn bezier_closed_two_anchors_makes_two_segments() {
        // N=2 with closed=true: outgoing segment 0→1 + wrap segment
        // 1→0. Without the fix, t=1 lands on anchors[1]; with the fix
        // it lands on anchors[0].
        let a0 = Vec3::ZERO;
        let a1 = Vec3::new(2.0, 0.0, 0.0);
        let curve = BezierCurve {
            anchors: vec![a0, a1],
            handles: vec![a0, a1, a1, a0],
            closed: true,
        };
        // Halfway through the curve should be the second anchor
        // (end of segment 0, start of segment 1).
        assert!(curve.point_at(0.5).abs_diff_eq(a1, 1.0e-5));
        // End of the loop returns to a0.
        assert!(curve.point_at(1.0).abs_diff_eq(a0, 1.0e-5));
    }

    #[test]
    fn frame_sequence_length_matches() {
        let curve = CatmullRomCurve::new(
            vec![
                Vec3::ZERO,
                Vec3::X,
                Vec3::X + Vec3::Y,
                Vec3::X + Vec3::Y + Vec3::Z,
            ],
            false,
        );
        let frames = FrameSequence::parallel_transport(&curve, 8, Vec3::Y);
        assert_eq!(frames.frames.len(), 8);
        for f in &frames.frames {
            let basis_ok =
                f.tangent.dot(f.normal).abs() < 1.0e-3 && f.tangent.dot(f.binormal).abs() < 1.0e-3;
            assert!(basis_ok);
        }
    }
}
