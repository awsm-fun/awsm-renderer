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
    casters_world_aabbs: &[Aabb],
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

    // Extend the cascade's light-view depth range to capture casters
    // between the light and the visible slice, then pull the near
    // plane back further by `Z_PULL_BACK_MIN` for floating-point
    // headroom — without that slack, a caster's topmost vertex
    // sitting *exactly* on `max.z` rasterises to `NDC.z ≈ 0` and
    // gets pinched at the near-plane by the hardware clip stage,
    // leaving a gap between the caster's contact point and the
    // start of its shadow on the ground.
    //
    // Caster filter: world-space cube of side `2·sphere_radius`
    // around the slice centre, then collect the clipped portion's
    // light-view Z extent. This excludes casters laterally far from
    // the cascade (especially long thin meshes like a 10 km ground
    // plane) so they don't inflate the cascade's Z range and ruin
    // shadow-map depth precision. Casters between the cube and the
    // cascade's true (infinite-along-light) prism footprint are
    // missed; the outer cascade — whose cube is larger — typically
    // covers them.
    let mut have_caster = false;
    let mut cmin_z = f32::INFINITY;
    let mut cmax_z = f32::NEG_INFINITY;
    if !casters_world_aabbs.is_empty() {
        let clip_min_w = frustum_center - Vec3::splat(sphere_radius);
        let clip_max_w = frustum_center + Vec3::splat(sphere_radius);
        for aabb in casters_world_aabbs {
            let clipped_min = Vec3::new(
                aabb.min.x.max(clip_min_w.x),
                aabb.min.y.max(clip_min_w.y),
                aabb.min.z.max(clip_min_w.z),
            );
            let clipped_max = Vec3::new(
                aabb.max.x.min(clip_max_w.x),
                aabb.max.y.min(clip_max_w.y),
                aabb.max.z.min(clip_max_w.z),
            );
            if clipped_min.x > clipped_max.x
                || clipped_min.y > clipped_max.y
                || clipped_min.z > clipped_max.z
            {
                continue;
            }
            let corners = [
                Vec3::new(clipped_min.x, clipped_min.y, clipped_min.z),
                Vec3::new(clipped_max.x, clipped_min.y, clipped_min.z),
                Vec3::new(clipped_min.x, clipped_max.y, clipped_min.z),
                Vec3::new(clipped_max.x, clipped_max.y, clipped_min.z),
                Vec3::new(clipped_min.x, clipped_min.y, clipped_max.z),
                Vec3::new(clipped_max.x, clipped_min.y, clipped_max.z),
                Vec3::new(clipped_min.x, clipped_max.y, clipped_max.z),
                Vec3::new(clipped_max.x, clipped_max.y, clipped_max.z),
            ];
            for c in &corners {
                let ls = view.transform_point3(*c);
                cmin_z = cmin_z.min(ls.z);
                cmax_z = cmax_z.max(ls.z);
            }
            have_caster = true;
        }
    }
    if have_caster {
        max.z = max.z.max(cmax_z);
        min.z = min.z.min(cmin_z);
    }

    // `Mat4::orthographic_rh` expects positive distances along the
    // eye's -Z forward axis. Our light-view AABB stored view-space
    // z directly (negative for points in front of the eye), so
    // negate when converting to ortho-rh's "distance from eye"
    // convention. Then pull the near plane back by
    // `(visible_far - visible_near).max(Z_PULL_BACK_MIN)` for two
    // distinct reasons:
    //
    //   1. *Float-precision headroom at the near plane.* Casters
    //      whose top corner sat exactly on `max.z` would land at
    //      `NDC.z ≈ 0` and get clipped by the hardware near-plane,
    //      leaving a visible "peter-panning" gap between the
    //      caster and the start of its shadow.
    //   2. *Missed casters between the world cube and the true
    //      cascade prism.* The cube filter is conservative-from-
    //      the-receiver-side but can miss casters laterally
    //      outside the cube that still shadow into the cascade.
    //      A scene-scale pull-back gives those casters somewhere
    //      to land in depth.
    //
    // `Z_PULL_BACK_MIN` is a soft floor — when the caster-driven
    // range is already wider than this we don't shrink it.
    const Z_PULL_BACK_MIN: f32 = 50.0;
    let visible_near = -max.z;
    let visible_far = -min.z;
    let z_pull_back = (visible_far - visible_near).max(Z_PULL_BACK_MIN);
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
    casters_world_aabbs: &[Aabb],
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
            casters_world_aabbs,
        );
        cascades.push((cascade, res, *split_world));
        prev_split_world = *split_world;
        prev_span = span;
    }
    cascades
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── pssm_splits: the PSSM (parallel-split) cascade boundaries ──────────
    // These are the far-distances of each cascade; shadow quality depends on
    // them being well-formed, so lock the invariants.

    #[test]
    fn pssm_splits_count_is_clamped() {
        assert_eq!(pssm_splits(0.1, 100.0, 0.5, 3).len(), 3);
        assert_eq!(pssm_splits(0.1, 100.0, 0.5, 1).len(), 1);
        // 0 clamps up to 1; above MAX clamps down to MAX.
        assert_eq!(pssm_splits(0.1, 100.0, 0.5, 0).len(), 1);
        assert_eq!(
            pssm_splits(0.1, 100.0, 0.5, 99).len(),
            MAX_CASCADES,
            "cascade count clamps to MAX_CASCADES"
        );
    }

    #[test]
    fn pssm_splits_monotonic_increasing_and_last_is_far() {
        for lambda in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let s = pssm_splits(0.5, 200.0, lambda, 4);
            for w in s.windows(2) {
                assert!(
                    w[1] > w[0],
                    "splits must strictly increase (lambda={lambda}): {s:?}"
                );
            }
            let last = *s.last().unwrap();
            assert!(
                (last - 200.0).abs() < 1e-2,
                "last split must equal far (lambda={lambda}): got {last}"
            );
        }
    }

    #[test]
    fn pssm_splits_within_near_far() {
        let near = 0.3;
        let far = 150.0;
        let s = pssm_splits(near, far, 0.5, 4);
        for v in &s {
            assert!(
                *v > near - 1e-3 && *v <= far + 1e-2,
                "split {v} out of [{near}, {far}]"
            );
        }
    }

    #[test]
    fn pssm_splits_lambda0_is_uniform() {
        let (near, far, n) = (1.0_f32, 101.0_f32, 4u32);
        let s = pssm_splits(near, far, 0.0, n);
        for (i, v) in s.iter().enumerate() {
            let p = (i + 1) as f32 / n as f32;
            let expected = near + (far - near) * p;
            assert!(
                (v - expected).abs() < 1e-3,
                "lambda=0 must be uniform: split[{i}]={v} expected {expected}"
            );
        }
    }

    #[test]
    fn pssm_splits_lambda1_is_logarithmic() {
        let (near, far, n) = (1.0_f32, 256.0_f32, 4u32);
        let s = pssm_splits(near, far, 1.0, n);
        let ratio = far / near;
        for (i, v) in s.iter().enumerate() {
            let p = (i + 1) as f32 / n as f32;
            let expected = near * ratio.powf(p);
            assert!(
                (v - expected).abs() < 1e-2,
                "lambda=1 must be logarithmic: split[{i}]={v} expected {expected}"
            );
        }
    }

    #[test]
    fn pssm_splits_near_zero_is_finite() {
        // near <= 0 takes the `far * p` fallback — must not NaN/inf.
        let s = pssm_splits(0.0, 100.0, 0.5, 4);
        assert_eq!(s.len(), 4);
        for v in &s {
            assert!(v.is_finite(), "near=0 fallback produced non-finite {v}");
        }
        assert!((s.last().unwrap() - 100.0).abs() < 1e-2);
    }

    #[test]
    fn cascade_resolution_floors_at_min() {
        assert_eq!(cascade_resolution(2048, 0, 16), 2048);
        assert_eq!(cascade_resolution(8, 0, 16), 16, "below min floors to min");
        assert_eq!(cascade_resolution(16, 3, 16), 16);
    }

    // ── fit_cascades: structural smoke test with real matrices ─────────────
    #[test]
    fn fit_cascades_count_ordering_and_far() {
        let near = 0.1_f32;
        let far = 100.0_f32;
        let count = 4u32;
        let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 16.0 / 9.0, near, far);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 5.0, 10.0), Vec3::ZERO, Vec3::Y);
        let view_proj = proj * view;
        let dir = Vec3::new(0.3, -1.0, 0.2).normalize();
        let out = fit_cascades(view_proj, view, dir, near, far, count, 0.5, 2048, 16, &[]);
        assert_eq!(out.len(), count as usize, "one entry per cascade");
        // split_far (the f32) must strictly increase and end at far.
        let fars: Vec<f32> = out.iter().map(|(_, _, f)| *f).collect();
        for w in fars.windows(2) {
            assert!(w[1] > w[0], "cascade split_far must increase: {fars:?}");
        }
        assert!((fars.last().unwrap() - far).abs() < 1e-1);
        // Every cascade must produce finite matrices + positive texel size.
        for (c, res, _) in &out {
            assert!(*res >= 16);
            assert!(c.world_per_texel > 0.0 && c.world_per_texel.is_finite());
            assert!(c
                .view_projection
                .to_cols_array()
                .iter()
                .all(|v| v.is_finite()));
        }
    }
}
