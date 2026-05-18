//! CSM cascade fitting for directional lights.
//!
//! `fit_cascade` is the per-split workhorse — given a normalised
//! near/far in NDC depth space, it produces an orthographic light
//! view-projection that tightly encloses the slice. `pssm_splits` /
//! `fit_cascades` build the per-light cascade set from a count + PSSM
//! lambda + base resolution.

use glam::{Mat4, Vec3, Vec4};

/// Maximum number of cascades per directional light, matching the
/// schema's `cascade_count: u8` valid range.
pub const MAX_CASCADES: usize = 4;

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
    let up = if dir.x.abs() < 0.9 { Vec3::X } else { Vec3::Z };

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

/// Computes the cascade far-distances in **world-space depth** using
/// the Practical Split Scheme for Parallel-Split Shadow Maps (PSSM):
/// blend between a uniform split (`lambda = 0`) and a logarithmic
/// split (`lambda = 1`).
///
/// Returns `cascade_count` values; the last one equals `far`.
/// `near` / `far` are the camera's view-space near and far planes.
pub fn pssm_splits(near: f32, far: f32, lambda: f32, cascade_count: u32) -> Vec<f32> {
    let n = cascade_count.max(1).min(MAX_CASCADES as u32);
    let ratio = if near > 0.0 { far / near } else { 1.0 };
    let mut splits = Vec::with_capacity(n as usize);
    for i in 1..=n {
        let p = i as f32 / n as f32;
        let log_split = if near > 0.0 {
            near * ratio.powf(p)
        } else {
            far * p
        };
        let uniform_split = near + (far - near) * p;
        let split = lambda * log_split + (1.0 - lambda) * uniform_split;
        splits.push(split);
    }
    splits
}

/// Per-cascade resolution: `max(min_res, base >> i)`. Phase 4 uses
/// this to halve the resolution for each successively-far cascade,
/// trading distant precision for memory bandwidth.
pub fn cascade_resolution(base: u32, cascade_index: u32, min_res: u32) -> u32 {
    (base >> cascade_index).max(min_res)
}

/// Convenience: fit every cascade for a directional light in one call.
/// Returns one [`Cascade`] per requested cascade.
///
/// `world_near` / `world_far` are the camera's view-space near and
/// far planes; the cascades partition that range using
/// [`pssm_splits`].
pub fn fit_cascades(
    camera_view_projection: Mat4,
    camera_view: Mat4,
    direction: Vec3,
    world_near: f32,
    world_far: f32,
    cascade_count: u32,
    lambda: f32,
    base_resolution: u32,
    min_resolution: u32,
) -> Vec<(Cascade, u32, f32)> {
    let inv_view_proj = camera_view_projection.inverse();
    let splits = pssm_splits(world_near, world_far, lambda, cascade_count);

    // Convert world-space splits to NDC z. Project a view-space point
    // at the split's z onto clip space, then divide by w to get NDC.z.
    let proj = camera_view_projection * camera_view.inverse();
    let split_to_ndc = |z: f32| {
        let view_p = Vec4::new(0.0, 0.0, -z, 1.0);
        let clip = proj * view_p;
        if clip.w.abs() < 1e-8 {
            return 1.0;
        }
        (clip.z / clip.w).clamp(0.0, 1.0)
    };

    let mut prev_ndc = 0.0;
    let mut cascades = Vec::with_capacity(splits.len());
    for (i, split_world) in splits.iter().enumerate() {
        let ndc_far = split_to_ndc(*split_world);
        let res = cascade_resolution(base_resolution, i as u32, min_resolution);
        let cascade = fit_cascade(inv_view_proj, direction, prev_ndc, ndc_far, res);
        cascades.push((cascade, res, *split_world));
        prev_ndc = ndc_far;
    }
    cascades
}
