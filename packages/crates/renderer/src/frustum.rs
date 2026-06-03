//! Frustum extraction and culling helpers.

use glam::{Mat4, Vec3, Vec4};

use crate::bounds::Aabb;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Plane {
    pub(crate) normal: Vec3,
    pub(crate) d: f32,
}

impl Plane {
    pub(crate) fn from_row(row: Vec4) -> Self {
        let normal = Vec3::new(row.x, row.y, row.z);
        let d = row.w;
        let len = normal.length();
        if len > 0.0 {
            Self {
                normal: normal / len,
                d: d / len,
            }
        } else {
            Self { normal, d }
        }
    }

    pub(crate) fn distance(&self, point: Vec3) -> f32 {
        self.normal.dot(point) + self.d
    }
}

/// View frustum planes extracted from a view-projection matrix.
#[derive(Debug, Clone, Copy)]
pub struct Frustum {
    pub(crate) planes: [Plane; 6],
}

impl Frustum {
    // Assumes a right-handed view-projection with WebGPU depth range [0, 1].
    /// Builds a frustum from a view-projection matrix.
    pub fn from_view_projection(view_projection: Mat4) -> Self {
        let x = view_projection.x_axis;
        let y = view_projection.y_axis;
        let z = view_projection.z_axis;
        let w = view_projection.w_axis;

        let row0 = Vec4::new(x.x, y.x, z.x, w.x);
        let row1 = Vec4::new(x.y, y.y, z.y, w.y);
        let row2 = Vec4::new(x.z, y.z, z.z, w.z);
        let row3 = Vec4::new(x.w, y.w, z.w, w.w);

        let left = Plane::from_row(row3 + row0);
        let right = Plane::from_row(row3 - row0);
        let bottom = Plane::from_row(row3 + row1);
        let top = Plane::from_row(row3 - row1);
        let near = Plane::from_row(row2);
        let far = Plane::from_row(row3 - row2);

        Self {
            planes: [left, right, bottom, top, near, far],
        }
    }

    /// Returns true if the AABB intersects the frustum.
    pub fn intersects_aabb(&self, aabb: &Aabb) -> bool {
        for plane in &self.planes {
            let px = if plane.normal.x >= 0.0 {
                aabb.max.x
            } else {
                aabb.min.x
            };
            let py = if plane.normal.y >= 0.0 {
                aabb.max.y
            } else {
                aabb.min.y
            };
            let pz = if plane.normal.z >= 0.0 {
                aabb.max.z
            } else {
                aabb.min.z
            };
            let p = Vec3::new(px, py, pz);
            if plane.distance(p) < 0.0 {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests;
