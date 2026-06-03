//! Axis-aligned bounding box type and predicates.

use glam::Vec3;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    pub fn empty() -> Self {
        Self {
            min: Vec3::splat(f32::INFINITY),
            max: Vec3::splat(f32::NEG_INFINITY),
        }
    }

    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Self {
        let mut aabb = Self::empty();
        for p in points {
            aabb = aabb.extend_point(p);
        }
        aabb
    }

    pub fn extend_point(self, p: Vec3) -> Self {
        Self {
            min: self.min.min(p),
            max: self.max.max(p),
        }
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }
}

pub fn point_in_aabb(p: Vec3, aabb: &Aabb) -> bool {
    p.x >= aabb.min.x
        && p.x <= aabb.max.x
        && p.y >= aabb.min.y
        && p.y <= aabb.max.y
        && p.z >= aabb.min.z
        && p.z <= aabb.max.z
}

pub fn aabb_overlap(a: &Aabb, b: &Aabb) -> bool {
    !(a.max.x < b.min.x
        || a.min.x > b.max.x
        || a.max.y < b.min.y
        || a.min.y > b.max.y
        || a.max.z < b.min.z
        || a.min.z > b.max.z)
}

pub fn aabb_union(a: &Aabb, b: &Aabb) -> Aabb {
    Aabb {
        min: a.min.min(b.min),
        max: a.max.max(b.max),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_in_aabb_basic() {
        let a = Aabb::new(Vec3::ZERO, Vec3::splat(1.0));
        assert!(point_in_aabb(Vec3::splat(0.5), &a));
        assert!(!point_in_aabb(Vec3::splat(2.0), &a));
    }

    #[test]
    fn overlap_detects_touching() {
        let a = Aabb::new(Vec3::ZERO, Vec3::splat(1.0));
        let b = Aabb::new(Vec3::splat(0.5), Vec3::splat(2.0));
        assert!(aabb_overlap(&a, &b));
        let c = Aabb::new(Vec3::splat(2.0), Vec3::splat(3.0));
        assert!(!aabb_overlap(&a, &c));
    }

    #[test]
    fn union_extends_bounds() {
        let a = Aabb::new(Vec3::ZERO, Vec3::splat(1.0));
        let b = Aabb::new(Vec3::splat(-1.0), Vec3::splat(0.5));
        let u = aabb_union(&a, &b);
        assert_eq!(u.min, Vec3::splat(-1.0));
        assert_eq!(u.max, Vec3::splat(1.0));
    }
}
