use awsm_renderer::bounds::Aabb;
use glam::Vec3;

use crate::pages::app::scene::camera::CameraView;

const MIN_NEAR: f32 = 0.001;
const MIN_RANGE: f32 = 0.1;
const MAX_DEPTH_RATIO: f32 = 1_000_000.0;
// Conservative safety margin for fallback far plane when bounds are suspicious.
// Large enough to avoid accidental clipping from camera drift, but still bounded by MAX_DEPTH_RATIO.
const CONSERVATIVE_FAR_RADIUS_MULTIPLIER: f32 = 256.0;

pub(super) fn tight_clip_planes_from_aabb(
    view: &CameraView,
    aabb: &Aabb,
    margin: f32,
) -> (f32, f32) {
    let view_matrix = view.view_matrix();

    let corners = [
        Vec3::new(aabb.min.x, aabb.min.y, aabb.min.z),
        Vec3::new(aabb.max.x, aabb.min.y, aabb.min.z),
        Vec3::new(aabb.min.x, aabb.max.y, aabb.min.z),
        Vec3::new(aabb.max.x, aabb.max.y, aabb.min.z),
        Vec3::new(aabb.min.x, aabb.min.y, aabb.max.z),
        Vec3::new(aabb.max.x, aabb.min.y, aabb.max.z),
        Vec3::new(aabb.min.x, aabb.max.y, aabb.max.z),
        Vec3::new(aabb.max.x, aabb.max.y, aabb.max.z),
    ];

    // Camera looks down -Z in view space, so forward distance is -z.
    let mut min_d = f32::INFINITY;
    let mut max_d = f32::NEG_INFINITY;
    for corner in &corners {
        let v = view_matrix.transform_point3(*corner);
        let d = -v.z;
        min_d = min_d.min(d);
        max_d = max_d.max(d);
    }

    let has_finite_range = min_d.is_finite() && max_d.is_finite();
    let has_front_geometry = max_d > MIN_NEAR;
    let has_tight_near = min_d > MIN_NEAR;
    let use_tight_range = has_finite_range && has_front_geometry;

    let (mut near, mut far) = if use_tight_range {
        let center = (min_d + max_d) * 0.5;
        let half = ((max_d - min_d) * 0.5 * margin).max(0.001);
        let near = (center - half).max(MIN_NEAR);
        let far = (center + half).max(near + MIN_RANGE);
        (near, far)
    } else {
        (MIN_NEAR, MIN_NEAR + MIN_RANGE)
    };

    // Conservative fallback for suspicious bounds to avoid visible clipping.
    // Keep the tighter range in the common case for better depth precision.
    if !use_tight_range || !has_tight_near {
        let view_distance = (view.position() - view.look_at()).length();
        let scene_radius = (aabb.size().length() * 0.5 * margin.max(1.0)).max(1.0);
        let conservative_far =
            (view_distance + scene_radius * CONSERVATIVE_FAR_RADIUS_MULTIPLIER).max(10_000.0);
        near = MIN_NEAR;
        far = far.max(conservative_far).max(near + MIN_RANGE);
    }

    let ratio_near = far / MAX_DEPTH_RATIO;
    if near < ratio_near {
        near = ratio_near.max(MIN_NEAR);
    }

    (near, far)
}
