//! Ray type + ray/AABB, ray/triangle (Möller–Trumbore), ray/plane intersection.

use glam::Vec3;

use crate::aabb::Aabb;

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Ray {
    pub origin: Vec3,
    pub direction: Vec3,
}

impl Ray {
    pub fn new(origin: Vec3, direction: Vec3) -> Self {
        Self { origin, direction: direction.normalize_or_zero() }
    }

    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.direction * t
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RayHit {
    pub t: f32,
    pub point: Vec3,
}

/// Slab test for ray vs AABB. Returns `(t_near, t_far)` if hit, else `None`.
pub fn ray_aabb(ray: &Ray, aabb: &Aabb) -> Option<(f32, f32)> {
    let inv_d = Vec3::new(
        if ray.direction.x.abs() > 1.0e-20 { 1.0 / ray.direction.x } else { f32::INFINITY },
        if ray.direction.y.abs() > 1.0e-20 { 1.0 / ray.direction.y } else { f32::INFINITY },
        if ray.direction.z.abs() > 1.0e-20 { 1.0 / ray.direction.z } else { f32::INFINITY },
    );
    let t0 = (aabb.min - ray.origin) * inv_d;
    let t1 = (aabb.max - ray.origin) * inv_d;
    let t_min_vec = t0.min(t1);
    let t_max_vec = t0.max(t1);
    let t_near = t_min_vec.max_element();
    let t_far = t_max_vec.min_element();
    if t_far < 0.0 || t_near > t_far {
        None
    } else {
        Some((t_near.max(0.0), t_far))
    }
}

/// Möller–Trumbore ray/triangle intersection. Returns `t` along the ray if the hit is
/// in front of the origin, else `None`.
pub fn ray_triangle(ray: &Ray, a: Vec3, b: Vec3, c: Vec3) -> Option<RayHit> {
    let e1 = b - a;
    let e2 = c - a;
    let h = ray.direction.cross(e2);
    let det = e1.dot(h);
    if det.abs() < 1.0e-7 {
        return None;
    }
    let inv_det = 1.0 / det;
    let s = ray.origin - a;
    let u = inv_det * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = inv_det * ray.direction.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = inv_det * e2.dot(q);
    if t > 1.0e-6 {
        Some(RayHit { t, point: ray.at(t) })
    } else {
        None
    }
}

/// Ray/plane intersection. Plane is `dot(normal, p) = d`. Returns `t` if hit in front.
pub fn ray_plane(ray: &Ray, normal: Vec3, d: f32) -> Option<RayHit> {
    let n_dot_d = normal.dot(ray.direction);
    if n_dot_d.abs() < 1.0e-7 {
        return None;
    }
    let t = (d - normal.dot(ray.origin)) / n_dot_d;
    if t < 0.0 {
        None
    } else {
        Some(RayHit { t, point: ray.at(t) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_aabb_hits_unit_cube() {
        let r = Ray::new(Vec3::new(-2.0, 0.5, 0.5), Vec3::X);
        let a = Aabb::new(Vec3::ZERO, Vec3::splat(1.0));
        let hit = ray_aabb(&r, &a).expect("should hit");
        assert!((hit.0 - 2.0).abs() < 1.0e-4);
    }

    #[test]
    fn ray_triangle_basic_hit() {
        let r = Ray::new(Vec3::new(0.25, 0.25, -1.0), Vec3::Z);
        let a = Vec3::ZERO;
        let b = Vec3::new(1.0, 0.0, 0.0);
        let c = Vec3::new(0.0, 1.0, 0.0);
        let hit = ray_triangle(&r, a, b, c).expect("should hit");
        assert!((hit.t - 1.0).abs() < 1.0e-4);
    }

    #[test]
    fn ray_plane_floor() {
        let r = Ray::new(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y);
        let hit = ray_plane(&r, Vec3::Y, 0.0).expect("should hit");
        assert!((hit.t - 5.0).abs() < 1.0e-4);
    }
}
