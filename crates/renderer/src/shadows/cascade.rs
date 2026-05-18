//! CSM cascade fitting for directional lights.
//!
//! Phase 2 ships a single-cascade fitter. Phase 4 generalizes to
//! 1–4 cascades with PSSM splits + per-cascade resolution scaling and
//! re-uses [`fit_cascade`] per split.

use glam::{Mat4, Vec3, Vec4};

/// Output of fitting one cascade. `view_projection` is the light-space
/// transform applied at sample time and during shadow generation.
#[derive(Clone, Debug)]
pub struct Cascade {
    /// Light-space view matrix (looks down `-direction`).
    pub view: Mat4,
    /// Orthographic projection covering the cascade's frustum slice.
    pub projection: Mat4,
    /// `projection * view`.
    pub view_projection: Mat4,
}

/// Fits an orthographic light-space frustum to the camera's view
/// frustum between `near` and `far`. Stable-fit: the projection origin
/// is snapped to a texel grid so the cascade does not swim when the
/// camera rotates.
///
/// `camera_inv_view_projection` is the inverse of the rendering
/// camera's `view * projection`. The 8 NDC corners (z in `[0, 1]` for
/// WebGPU) are unprojected through it to obtain the world-space
/// frustum corners.
///
/// `direction` is the light's *forward* direction (light travels along
/// this vector). The light view looks from `-direction` toward the
/// scene.
///
/// `resolution` is the cascade's atlas resolution in texels — needed
/// for the stable-fit snap.
pub fn fit_cascade(
    camera_inv_view_projection: Mat4,
    direction: Vec3,
    near_normalized: f32,
    far_normalized: f32,
    resolution: u32,
) -> Cascade {
    // Eight NDC corners of the camera's view frustum, parameterised by
    // the slice's near/far z in NDC. WebGPU NDC z is [0, 1] (near = 0,
    // far = 1) — we interpolate between those.
    let z_near = near_normalized.clamp(0.0, 1.0);
    let z_far = far_normalized.clamp(0.0, 1.0);
    let ndc_corners = [
        Vec4::new(-1.0, -1.0, z_near, 1.0),
        Vec4::new(1.0, -1.0, z_near, 1.0),
        Vec4::new(-1.0, 1.0, z_near, 1.0),
        Vec4::new(1.0, 1.0, z_near, 1.0),
        Vec4::new(-1.0, -1.0, z_far, 1.0),
        Vec4::new(1.0, -1.0, z_far, 1.0),
        Vec4::new(-1.0, 1.0, z_far, 1.0),
        Vec4::new(1.0, 1.0, z_far, 1.0),
    ];

    let mut world_corners = [Vec3::ZERO; 8];
    let mut center = Vec3::ZERO;
    for (i, c) in ndc_corners.iter().enumerate() {
        let world = camera_inv_view_projection * *c;
        let w = if world.w.abs() < 1e-8 { 1.0 } else { world.w };
        world_corners[i] = Vec3::new(world.x / w, world.y / w, world.z / w);
        center += world_corners[i];
    }
    center *= 1.0 / 8.0;

    // Build the light view that looks down `direction`. We position the
    // light "eye" at the frustum-slice centroid offset back along
    // -direction; the actual eye position cancels out as the
    // orthographic projection's depth range is set explicitly below,
    // but using the centroid keeps the numerical magnitudes well-bounded.
    let dir = if direction.length_squared() < 1e-8 {
        Vec3::new(0.0, -1.0, 0.0)
    } else {
        direction.normalize()
    };
    let up = if dir.x.abs() < 0.9 {
        Vec3::X
    } else {
        Vec3::Z
    };

    let view = Mat4::look_at_rh(center - dir, center, up);

    // AABB of frustum corners in light space.
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for c in &world_corners {
        let ls = view.transform_point3(*c);
        min = min.min(ls);
        max = max.max(ls);
    }

    // Stable-fit snap: round `min.xy` to the nearest texel size in
    // light space so the projection origin moves in texel increments
    // when the camera rotates / dollies. Without this the cascade
    // texels would rasterise to slightly different world locations
    // each frame and shadow edges would crawl ("swimming").
    let extents = max - min;
    let texel_size_x = extents.x / resolution as f32;
    let texel_size_y = extents.y / resolution as f32;
    if texel_size_x > 0.0 {
        min.x = (min.x / texel_size_x).floor() * texel_size_x;
        max.x = min.x + extents.x;
    }
    if texel_size_y > 0.0 {
        min.y = (min.y / texel_size_y).floor() * texel_size_y;
        max.y = min.y + extents.y;
    }

    // Pull the near plane back along the light's forward axis so
    // off-screen casters (e.g. tall towers behind the camera) still
    // cast shadows into the cascade. A fixed expansion is sufficient
    // for v1; phase 12 may make this a per-cascade tuning knob.
    let z_pull_back = (max.z - min.z).max(50.0);
    let near = min.z - z_pull_back;
    let far = max.z;

    let projection = Mat4::orthographic_rh(min.x, max.x, min.y, max.y, near, far);
    let view_projection = projection * view;

    Cascade {
        view,
        projection,
        view_projection,
    }
}
