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
    /// Builds a frustum from a view-projection matrix. `reverse_z` MUST match
    /// the convention the projection was built under (003): the near/far
    /// clip-space halfspaces swap rows (forward: near is z>=0 i.e. row2, far
    /// is z<=w i.e. row3-row2; reverse: near is z<=w i.e. row3-row2, far is
    /// z>=0 i.e. row2). The extracted WORLD-space planes are identical either
    /// way -- only which row encodes which plane changes.
    pub fn from_view_projection(view_projection: Mat4, reverse_z: bool) -> Self {
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
        let (near, far) = if reverse_z {
            // Under INFINITE-far reverse-Z (the main-camera projection) the
            // far halfspace `z_clip >= 0` is satisfied by every finite point:
            // row2 degenerates to a zero normal with d = near·w > 0, i.e. an
            // always-pass plane — geometrically correct (nothing lies beyond
            // infinity). Normalize that case explicitly so a future refactor
            // can't accidentally turn it into a cull-everything plane.
            let far_row = row2;
            let far = if far_row.truncate().length_squared() < 1e-12 {
                Plane {
                    normal: Vec3::ZERO,
                    d: f32::MAX,
                }
            } else {
                Plane::from_row(far_row)
            };
            (Plane::from_row(row3 - row2), far)
        } else {
            (Plane::from_row(row2), Plane::from_row(row3 - row2))
        };

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
