//! The renderer's depth convention — forward-Z (near→0, far→1) vs
//! REVERSE-Z (near→1, far→0) — as one value every producer reads
//! (plan 003-reverse-z, deleted as shipped — git history).
//!
//! Reverse-Z pairs the reversed depth distribution with float32's exponent
//! bunching near 0.0, cancelling perspective's far-field precision starvation
//! to near-uniform precision. Everything that touches depth derives from this
//! ONE value: projection builders, depth clears, compare directions, HZB
//! reduce ops, frustum-plane extraction, and background sentinels. Flipping a
//! subset silently over/under-culls or mis-renders — never hardcode a depth
//! constant in a main-camera path; read the convention.
//!
//! Shadows follow the SAME convention (stage-7 lockstep migration):
//! writer projections, the comparison sampler, caster pipeline
//! compare + rasterizer bias, the depth clear, the receiver's NDC-z
//! reconstruction, and the EVSM remap all read this one value and flip
//! together — see [`crate::shadows::Shadows::depth`].

use awsm_renderer_core::compare::CompareFunction;

/// The active depth convention. Copy — pass by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DepthConvention {
    /// `true` = reverse-Z (near→1, far→0, clear 0.0, GreaterEqual, background
    /// sentinel `depth <= 0.0`, "closer" = larger).
    pub reverse_z: bool,
}

impl DepthConvention {
    /// The classic forward-Z convention (near→0/far→1). For tests and
    /// placeholder sites that must stay convention-independent.
    pub const FORWARD: Self = Self { reverse_z: false };

    /// Depth-buffer clear value = the FARTHEST depth (background sentinel).
    pub fn clear_value(self) -> f32 {
        if self.reverse_z {
            0.0
        } else {
            1.0
        }
    }

    /// Depth test for "closer or equal wins" (the standard opaque test).
    pub fn compare(self) -> CompareFunction {
        if self.reverse_z {
            CompareFunction::GreaterEqual
        } else {
            CompareFunction::LessEqual
        }
    }

    /// Strict variant of [`Self::compare`] (the few `Less`/`Greater` sites).
    pub fn compare_strict(self) -> CompareFunction {
        if self.reverse_z {
            CompareFunction::Greater
        } else {
            CompareFunction::Less
        }
    }

    /// Whether `depth` is the background/sky sentinel (carries the clear
    /// value). WGSL consumers get the equivalent branch via their
    /// `reverse_z` template axis; keep the two in lockstep.
    pub fn is_background(self, depth: f32) -> bool {
        if self.reverse_z {
            depth <= 0.0
        } else {
            depth >= 1.0
        }
    }

    /// The NEAREST possible depth value ("closest" extreme) — reverse of the
    /// clear value. HZB/min-max reductions initialize "find the nearest"
    /// scans from the FARTHEST ([`Self::clear_value`]) and "find the
    /// farthest" scans from this.
    pub fn nearest_value(self) -> f32 {
        if self.reverse_z {
            1.0
        } else {
            0.0
        }
    }

    /// The NDC z of the NEAR plane under this convention (forward → 0,
    /// reverse → 1). Screen-space reconstruction that unprojects "at the near
    /// plane" must use this, not a literal 0 — under reverse-Z, z=0 is the FAR
    /// plane (at infinity once stage 8 lands, where unprojecting it yields
    /// w=0 → NaN rays).
    pub fn near_ndc_z(self) -> f32 {
        if self.reverse_z {
            1.0
        } else {
            0.0
        }
    }

    /// Right-handed perspective projection under this convention ([0,1] NDC
    /// depth) for the MAIN camera. Reverse-Z uses the INFINITE-far form
    /// (`perspective_infinite_reverse_rh`) — maximum precision, near→1 and
    /// depth asymptotically →0 with distance; the authored `far` is ignored
    /// by the matrix but still carried on `CameraMatrices.far` for the
    /// froxel/cascade clamps. Consumers needing a REAL finite far plane
    /// (shadow maps bound their range) use [`Self::perspective_finite`].
    pub fn perspective(self, fov_y: f32, aspect: f32, near: f32, _far: f32) -> glam::Mat4 {
        if self.reverse_z {
            glam::Mat4::perspective_infinite_reverse_rh(fov_y, aspect, near)
        } else {
            glam::Mat4::perspective_rh(fov_y, aspect, near, _far)
        }
    }

    /// FINITE perspective under this convention — reverse-Z via the near/far
    /// swap (near→1, far→exactly 0). Shadow writers use this: a shadow map's
    /// far plane bounds the light range, so infinite-far would break the
    /// receiver's analytic NDC reconstruction.
    pub fn perspective_finite(self, fov_y: f32, aspect: f32, near: f32, far: f32) -> glam::Mat4 {
        if self.reverse_z {
            glam::Mat4::perspective_rh(fov_y, aspect, far, near)
        } else {
            glam::Mat4::perspective_rh(fov_y, aspect, near, far)
        }
    }

    /// Right-handed orthographic projection under this convention. Ortho is
    /// inherently finite — reverse-Z just swaps near/far (near→1, far→0).
    #[allow(clippy::too_many_arguments)]
    pub fn orthographic(
        self,
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        near: f32,
        far: f32,
    ) -> glam::Mat4 {
        if self.reverse_z {
            glam::Mat4::orthographic_rh(left, right, bottom, top, far, near)
        } else {
            glam::Mat4::orthographic_rh(left, right, bottom, top, near, far)
        }
    }

    /// `true` when depth `a` is closer to the camera than `b`.
    pub fn is_closer(self, a: f32, b: f32) -> bool {
        if self.reverse_z {
            a > b
        } else {
            a < b
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_and_reverse_are_exact_mirrors() {
        let f = DepthConvention { reverse_z: false };
        let r = DepthConvention { reverse_z: true };
        assert_eq!(f.clear_value(), 1.0);
        assert_eq!(r.clear_value(), 0.0);
        assert_eq!(f.compare(), CompareFunction::LessEqual);
        assert_eq!(r.compare(), CompareFunction::GreaterEqual);
        assert_eq!(f.compare_strict(), CompareFunction::Less);
        assert_eq!(r.compare_strict(), CompareFunction::Greater);
        assert!(f.is_background(1.0) && !f.is_background(0.5));
        assert!(r.is_background(0.0) && !r.is_background(0.5));
        assert!(f.is_closer(0.1, 0.9) && !f.is_closer(0.9, 0.1));
        assert!(r.is_closer(0.9, 0.1) && !r.is_closer(0.1, 0.9));
        assert_eq!(f.nearest_value(), 0.0);
        assert_eq!(r.nearest_value(), 1.0);
        assert_eq!(f.near_ndc_z(), 0.0);
        assert_eq!(r.near_ndc_z(), 1.0);
    }

    /// Project a point at a given view-space depth and return NDC z.
    fn ndc_z(proj: glam::Mat4, view_z: f32) -> f32 {
        let clip = proj * glam::Vec4::new(0.0, 0.0, view_z, 1.0);
        clip.z / clip.w
    }

    #[test]
    fn reverse_perspective_maps_near_to_one_far_to_zero() {
        let f = DepthConvention { reverse_z: false };
        let r = DepthConvention { reverse_z: true };
        let (near, far) = (0.5, 100.0);
        let pf = f.perspective(1.0, 1.0, near, far);
        let pr = r.perspective(1.0, 1.0, near, far);
        // RH view space looks down -Z: the near plane is at view z = -near.
        assert!((ndc_z(pf, -near) - 0.0).abs() < 1e-5);
        assert!((ndc_z(pf, -far) - 1.0).abs() < 1e-5);
        assert!((ndc_z(pr, -near) - 1.0).abs() < 1e-5);
        // INFINITE-far reverse: depth decays asymptotically toward 0 with
        // distance — small and positive at the authored far, smaller further.
        let at_far = ndc_z(pr, -far);
        assert!(at_far > 0.0 && at_far < 0.01, "at_far = {at_far}");
        assert!(ndc_z(pr, -far * 100.0) < at_far);
        // Midpoint ordering flips: closer = larger depth under reverse.
        assert!(ndc_z(pr, -1.0) > ndc_z(pr, -10.0));
        assert!(ndc_z(pf, -1.0) < ndc_z(pf, -10.0));
    }

    #[test]
    fn finite_reverse_perspective_maps_far_to_exactly_zero() {
        let r = DepthConvention { reverse_z: true };
        let (near, far) = (0.5, 100.0);
        let p = r.perspective_finite(1.0, 1.0, near, far);
        assert!((ndc_z(p, -near) - 1.0).abs() < 1e-5);
        assert!((ndc_z(p, -far) - 0.0).abs() < 1e-5);
    }

    #[test]
    fn reverse_orthographic_maps_near_to_one_far_to_zero() {
        let r = DepthConvention { reverse_z: true };
        let po = r.orthographic(-1.0, 1.0, -1.0, 1.0, 0.5, 100.0);
        assert!((ndc_z(po, -0.5) - 1.0).abs() < 1e-5);
        assert!((ndc_z(po, -100.0) - 0.0).abs() < 1e-5);
    }
}
