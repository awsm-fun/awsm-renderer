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
}
