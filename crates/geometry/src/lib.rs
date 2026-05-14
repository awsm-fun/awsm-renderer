//! Pure-CPU non-curve geometry utilities. See [`README.md`](../README.md).

pub mod aabb;
pub mod frustum;
pub mod ray;

pub use aabb::{aabb_overlap, aabb_union, point_in_aabb, Aabb};
pub use frustum::{aabb_in_frustum, point_in_frustum, Frustum, FrustumPlane};
pub use ray::{ray_aabb, ray_plane, ray_triangle, Ray, RayHit};
