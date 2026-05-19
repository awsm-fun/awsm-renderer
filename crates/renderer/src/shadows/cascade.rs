//! CSM cascade fitting for directional lights.
//!
//! `fit_cascade` is the per-split workhorse — given a normalised
//! near/far in NDC depth space, it produces an orthographic light
//! view-projection that tightly encloses the slice. `pssm_splits` /
//! `fit_cascades` build the per-light cascade set from a count + PSSM
//! lambda + base resolution.

use glam::{Mat4, Vec3, Vec4};

use crate::bounds::Aabb;

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
    /// World-space distance spanned by a single shadow-map texel for
    /// this cascade. Equals `diameter / resolution`. Passed to the
    /// receiver so the PCF kernel can scale its texel-radius inversely
    /// to keep the perceived soft-edge width constant across cascades
    /// — without this scaling, the near cascade looks razor-sharp and
    /// the next one out is several times softer, which reveals the
    /// cascade boundary as a visible step in shadow penumbra width.
    pub world_per_texel: f32,
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
    caster_world_aabb: Option<&Aabb>,
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
    let mut frustum_center = Vec3::ZERO;
    for (i, c) in ndc_corners.iter().enumerate() {
        let world = camera_inv_view_projection * *c;
        let w = if world.w.abs() < 1e-8 { 1.0 } else { world.w };
        world_corners[i] = Vec3::new(world.x / w, world.y / w, world.z / w);
        frustum_center += world_corners[i];
    }
    frustum_center *= 1.0 / 8.0;

    // Bounding sphere of the slice corners. Its radius is invariant
    // under camera rotation (any rigid rotation around the slice
    // centroid leaves the corner distances unchanged), which gives
    // us a fixed cascade size — the prerequisite for stable-fit. An
    // AABB-derived size would dilate and contract as the camera spun,
    // so the texel grid would resize each frame and shadow edges
    // would crawl ("swimming"). The radius is rounded up to a coarse
    // step so it stays constant across small zoom changes too.
    let mut sphere_radius = 0.0_f32;
    for c in &world_corners {
        sphere_radius = sphere_radius.max((*c - frustum_center).length());
    }
    let diameter = sphere_radius * 2.0;

    let dir = if direction.length_squared() < 1e-8 {
        Vec3::new(0.0, -1.0, 0.0)
    } else {
        direction.normalize()
    };
    let up = if dir.x.abs() < 0.9 { Vec3::X } else { Vec3::Z };

    let view = Mat4::look_at_rh(frustum_center - dir, frustum_center, up);

    // The slice's bounding-sphere center in light view. The cascade is
    // a `diameter × diameter` square centered here in XY; depth bounds
    // start from this point and get widened by the caster AABB below.
    let center_ls = view.transform_point3(frustum_center);

    // Snap the cascade origin to a texel grid in light space. This is
    // the second half of the stable-fit recipe — even with a constant
    // diameter, sub-texel shifts in `min.xy` would cause each frame's
    // texel to cover a slightly different world strip, producing
    // crawling shadow edges as the camera translates.
    let texel_size = diameter / resolution as f32;
    let min_x = ((center_ls.x - sphere_radius) / texel_size).floor() * texel_size;
    let min_y = ((center_ls.y - sphere_radius) / texel_size).floor() * texel_size;
    let mut min = Vec3::new(min_x, min_y, center_ls.z - sphere_radius);
    let mut max = Vec3::new(
        min_x + diameter,
        min_y + diameter,
        center_ls.z + sphere_radius,
    );

    // Caster light-space AABB, used below to extend the cascade's
    // depth range so off-slice casters still rasterise.
    let mut caster_ls: Option<(Vec3, Vec3)> = None;
    if let Some(aabb) = caster_world_aabb {
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
        let mut cmin = Vec3::splat(f32::INFINITY);
        let mut cmax = Vec3::splat(f32::NEG_INFINITY);
        for c in &corners {
            let ls = view.transform_point3(*c);
            cmin = cmin.min(ls);
            cmax = cmax.max(ls);
        }
        caster_ls = Some((cmin, cmax));
    }

    if let Some((cmin, cmax)) = caster_ls {
        // Extend the cascade's light-view depth range to include the
        // full caster AABB. Casters whose light-view z falls outside
        // the receiver-frustum-slice's z range get NDC-clipped during
        // rasterisation and never appear in the depth atlas — even
        // though they may sit directly between the light and slice
        // receivers in WORLD space. Both ends matter:
        //   * `max.z` (closer to the light eye) captures the top of
        //     a tall caster that pokes up toward the light.
        //   * `min.z` (further from the light) captures the base of
        //     a caster that sits at receiver-similar depths.
        // XY is left as the stable-fit sphere bounds — extending it
        // would defeat the rotation-invariance and reintroduce swim.
        min.z = min.z.min(cmin.z);
        max.z = max.z.max(cmax.z);
    }

    // `Mat4::orthographic_rh` expects positive distances along the
    // eye's -Z forward axis: a `near` value of N means the near plane
    // is at `view_z = -N`. Our light-view AABB stored view-space z
    // directly (negative for points in front of the eye), so negate
    // when converting to ortho-rh's "distance from eye" convention.
    //
    // We then pull the near plane closer to the eye by
    // `z_pull_back` so casters between the light and the visible
    // cascade slice (above the visible scene from the light's POV)
    // still contribute to the depth map. Negative `near` is fine —
    // it just means the near plane is behind the eye.
    let visible_near = -max.z; // smallest distance from eye to scene
    let visible_far = -min.z; // largest distance from eye to scene
    let z_pull_back = (visible_far - visible_near).max(50.0);
    let near = visible_near - z_pull_back;
    let far = visible_far;

    let projection = Mat4::orthographic_rh(min.x, max.x, min.y, max.y, near, far);
    let view_projection = projection * view;

    // World-units-per-texel along the cascade's XY axes. After the
    // texel-snap above, `max.x - min.x` and `max.y - min.y` equal
    // `diameter` exactly; using the average keeps the formula
    // resilient if a future caller passes a non-square cascade.
    let avg_extent = ((max.x - min.x) + (max.y - min.y)) * 0.5;
    let world_per_texel = avg_extent / resolution as f32;

    Cascade {
        view,
        projection,
        view_projection,
        world_per_texel,
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

/// Per-cascade resolution. Every cascade gets the full `base`
/// resolution. Earlier versions halved per cascade index — that
/// trades memory for distance precision, but the per-cascade texel
/// size discontinuity is visible at split boundaries (closer
/// cascades look razor-sharp; the next one out is 2× softer), and
/// the seam survives any reasonable blend zone. Equal resolution is
/// the AAA default.
pub fn cascade_resolution(base: u32, _cascade_index: u32, min_res: u32) -> u32 {
    base.max(min_res)
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
    caster_world_aabb: Option<&Aabb>,
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

    // Each cascade's frustum is pulled BACK along its near edge by
    // `BLEND_OVERLAP` fractions of the *previous* cascade's depth
    // span. The shadow shader's `sample_shadow_directional` blends
    // the last `CASCADE_BLEND` fraction of every cascade into the
    // next cascade's sample to hide the resolution discontinuity at
    // splits — but that only works if the next cascade actually
    // covers the blend zone. Without the overlap below, the next
    // cascade's projection starts *at* the split boundary and the
    // blend would fade to "fully lit" instead of fading into the
    // next cascade's shadow.
    //
    // We slightly overshoot the shader's `CASCADE_BLEND` (currently
    // 0.5) so the next cascade unambiguously covers every blend-zone
    // receiver including those displaced by `normal_bias`. Must be
    // updated in lockstep if `CASCADE_BLEND` changes in WGSL.
    const BLEND_OVERLAP: f32 = 0.55;
    let mut cascades = Vec::with_capacity(splits.len());
    let mut prev_split_world = world_near;
    let mut prev_span = 0.0_f32;
    for (i, split_world) in splits.iter().enumerate() {
        let span = (*split_world - prev_split_world).max(0.0);
        let near_world = if i == 0 {
            prev_split_world
        } else {
            (prev_split_world - BLEND_OVERLAP * prev_span).max(world_near)
        };
        let ndc_near = split_to_ndc(near_world);
        let ndc_far = split_to_ndc(*split_world);
        let res = cascade_resolution(base_resolution, i as u32, min_resolution);
        let cascade = fit_cascade(
            inv_view_proj,
            direction,
            ndc_near,
            ndc_far,
            res,
            caster_world_aabb,
        );
        cascades.push((cascade, res, *split_world));
        prev_split_world = *split_world;
        prev_span = span;
    }
    cascades
}
