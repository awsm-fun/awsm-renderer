//! Axis-aligned bounding boxes.

use glam::{Mat4, Vec3};

/// Axis-aligned bounding box (AABB).
#[derive(Debug, Clone)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    /// Creates an AABB from min and max points.
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// Creates a cube AABB centered at the origin.
    pub const fn new_cube(width: f32, height: f32) -> Self {
        Self {
            min: Vec3::new(-width / 2.0, -height / 2.0, -width / 2.0),
            max: Vec3::new(width / 2.0, height / 2.0, width / 2.0),
        }
    }

    /// Creates a 2x2x2 cube AABB centered at the origin.
    pub const fn new_unit_cube() -> Self {
        Self::new_cube(2.0, 2.0)
    }

    /// Expands this AABB to include another.
    pub fn extend(&mut self, other: &Self) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    /// Transforms this AABB in place by a matrix.
    pub fn transform(&mut self, mat: &Mat4) {
        // Transform all 8 corners of the AABB and recompute bounds
        // This is necessary because rotation can change which corners are min/max
        let corners = [
            Vec3::new(self.min.x, self.min.y, self.min.z),
            Vec3::new(self.max.x, self.min.y, self.min.z),
            Vec3::new(self.min.x, self.max.y, self.min.z),
            Vec3::new(self.max.x, self.max.y, self.min.z),
            Vec3::new(self.min.x, self.min.y, self.max.z),
            Vec3::new(self.max.x, self.min.y, self.max.z),
            Vec3::new(self.min.x, self.max.y, self.max.z),
            Vec3::new(self.max.x, self.max.y, self.max.z),
        ];

        let first = mat.transform_point3(corners[0]);
        self.min = first;
        self.max = first;

        for corner in &corners[1..] {
            let transformed = mat.transform_point3(*corner);
            self.min = self.min.min(transformed);
            self.max = self.max.max(transformed);
        }
    }

    /// Returns a transformed copy of this AABB.
    pub fn transformed(&self, mat: &Mat4) -> Self {
        let mut out = self.clone();
        out.transform(mat);
        out
    }

    /// Returns the center point of the AABB.
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Returns the size of the AABB along each axis.
    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }
}

// glTF-specific AABB helpers (`Aabb::from_gltf_*`) moved to
// `awsm-renderer-gltf::aabb` so this crate no longer depends on the `gltf`
// crate. Callers should use `awsm_renderer_gltf::aabb_from_gltf_{doc,node,
// primitive}` instead.
