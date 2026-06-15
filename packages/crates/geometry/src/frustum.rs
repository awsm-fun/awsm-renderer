//! Frustum predicates.

use glam::{Mat4, Vec3, Vec4, Vec4Swizzles};

use crate::aabb::Aabb;

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FrustumPlane {
    /// Plane in form `n·p + d = 0`, where `n` is unit-length.
    pub normal: Vec3,
    pub d: f32,
}

impl FrustumPlane {
    pub fn signed_distance(&self, p: Vec3) -> f32 {
        self.normal.dot(p) + self.d
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Frustum {
    /// Six planes pointing inward: left, right, bottom, top, near, far.
    pub planes: [FrustumPlane; 6],
}

impl Frustum {
    /// Extract a frustum from a projection-view matrix (Gribb-Hartmann).
    ///
    /// CLIP-Z CONVENTION: the near/far planes are extracted for OpenGL clip
    /// space (NDC z in `[-1, 1]`) — near = `row3 + row2`, far = `row3 - row2`.
    /// A projection built for wgpu/D3D/Vulkan clip space (z in `[0, 1]`, e.g.
    /// `glam::Mat4::perspective_rh`) needs the near plane as `row2` alone, so
    /// feeding one here yields a too-permissive near plane. Pass a GL-convention
    /// projection (`perspective_rh_gl` / `orthographic_rh_gl`), or special-case
    /// the near plane, before using this for `[0,1]`-clip culling. The four side
    /// planes (left/right/top/bottom) are identical under both conventions.
    pub fn from_proj_view(m: Mat4) -> Self {
        let r0 = Vec4::new(m.x_axis.x, m.y_axis.x, m.z_axis.x, m.w_axis.x);
        let r1 = Vec4::new(m.x_axis.y, m.y_axis.y, m.z_axis.y, m.w_axis.y);
        let r2 = Vec4::new(m.x_axis.z, m.y_axis.z, m.z_axis.z, m.w_axis.z);
        let r3 = Vec4::new(m.x_axis.w, m.y_axis.w, m.z_axis.w, m.w_axis.w);

        let make = |v: Vec4| -> FrustumPlane {
            let n = v.xyz();
            let len = n.length();
            let inv = if len > 0.0 { 1.0 / len } else { 1.0 };
            FrustumPlane {
                normal: n * inv,
                d: v.w * inv,
            }
        };

        Self {
            planes: [
                make(r3 + r0),
                make(r3 - r0),
                make(r3 + r1),
                make(r3 - r1),
                make(r3 + r2),
                make(r3 - r2),
            ],
        }
    }
}

pub fn point_in_frustum(p: Vec3, f: &Frustum) -> bool {
    for plane in &f.planes {
        if plane.signed_distance(p) < 0.0 {
            return false;
        }
    }
    true
}

pub fn aabb_in_frustum(aabb: &Aabb, f: &Frustum) -> bool {
    // Positive-vertex test against each plane.
    for plane in &f.planes {
        let p = Vec3::new(
            if plane.normal.x >= 0.0 {
                aabb.max.x
            } else {
                aabb.min.x
            },
            if plane.normal.y >= 0.0 {
                aabb.max.y
            } else {
                aabb.min.y
            },
            if plane.normal.z >= 0.0 {
                aabb.max.z
            } else {
                aabb.min.z
            },
        );
        if plane.signed_distance(p) < 0.0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_inside_identity_clip() {
        // Identity proj-view means the unit cube [-1,1]^3 is the frustum.
        let f = Frustum::from_proj_view(Mat4::IDENTITY);
        assert!(point_in_frustum(Vec3::ZERO, &f));
        assert!(!point_in_frustum(Vec3::splat(5.0), &f));
    }

    // The identity proj-view's frustum is exactly the clip cube [-1,1]^3, which
    // is the same under either clip-Z convention — so these AABB-culling cases
    // are deterministic and convention-agnostic.
    fn unit_frustum() -> Frustum {
        Frustum::from_proj_view(Mat4::IDENTITY)
    }

    #[test]
    fn aabb_fully_inside_is_visible() {
        let a = Aabb::new(Vec3::splat(-0.5), Vec3::splat(0.5));
        assert!(aabb_in_frustum(&a, &unit_frustum()));
    }

    #[test]
    fn aabb_fully_outside_is_culled() {
        // Entirely beyond the +X plane (x in [2, 3]).
        let a = Aabb::new(Vec3::new(2.0, -0.5, -0.5), Vec3::new(3.0, 0.5, 0.5));
        assert!(!aabb_in_frustum(&a, &unit_frustum()));
    }

    #[test]
    fn aabb_straddling_plane_is_conservatively_visible() {
        // Crosses the +X = 1 boundary (x in [0.5, 1.5]); the positive-vertex test
        // must keep a partially-inside box visible (a false-negative would pop
        // geometry out of view at the screen edge).
        let a = Aabb::new(Vec3::new(0.5, -0.5, -0.5), Vec3::new(1.5, 0.5, 0.5));
        assert!(aabb_in_frustum(&a, &unit_frustum()));
    }

    #[test]
    fn aabb_just_beyond_far_plane_is_culled() {
        // z in [1.5, 2.0], past the far plane at z = 1.
        let a = Aabb::new(Vec3::new(-0.2, -0.2, 1.5), Vec3::new(0.2, 0.2, 2.0));
        assert!(!aabb_in_frustum(&a, &unit_frustum()));
    }

    #[test]
    fn point_beyond_near_and_far_is_outside() {
        let f = unit_frustum();
        assert!(
            !point_in_frustum(Vec3::new(0.0, 0.0, 2.0), &f),
            "beyond far z = 1"
        );
        assert!(
            !point_in_frustum(Vec3::new(0.0, 0.0, -2.0), &f),
            "before near z = -1"
        );
    }

    #[test]
    fn signed_distance_agrees_with_membership() {
        let f = unit_frustum();
        // An inside point is on the inward side of every plane.
        assert!(f
            .planes
            .iter()
            .all(|p| p.signed_distance(Vec3::ZERO) >= 0.0));
        // An outside point fails at least one plane.
        assert!(f
            .planes
            .iter()
            .any(|p| p.signed_distance(Vec3::new(5.0, 0.0, 0.0)) < 0.0));
    }
}
