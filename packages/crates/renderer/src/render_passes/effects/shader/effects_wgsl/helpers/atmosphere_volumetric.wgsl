// Composite the pre-integrated froxel volume over the scene, then continue the
// medium analytically past the volume's far plane.
//
// The froxel volume is a finite ACCELERATION STRUCTURE, not the extent of the
// air. It resolves the part of the medium worth resolving — near the camera,
// where beams and their shadows are visible — and stops. Everything past it is
// still full of the same medium, and until this pass grew the second term it
// simply wasn't: the composite clamped to the last slice, so the air ended at
// `volumetric_distance` and a knob documented as a cost/quality budget was
// silently an art knob. Dropping it 25 m -> 8 m to buy z-resolution visibly
// removed haze, which is exactly the coupling that shouldn't exist.
//
// This is NOT double-fogging. The two terms cover DISJOINT segments of the
// ray: the volume owns [near, volume_far], the analytic term owns
// [volume_far, surface]. Running both over the SAME segment is the
// extinguish-the-air-twice bug the mode switch exists to prevent, and it is
// worth re-reading this comment before touching either bound.
//
// Composite order follows the light: it leaves the surface, crosses the far
// (analytic) segment, then the near (froxel) one.
//
//     rgb_far  ->  rgb * T_a + color * (1 - T_a)      // analytic segment
//              ->  that * T_v + inscatter_v           // froxel segment
//
// The froxel half stores, per froxel, the in-scatter accumulated from the eye
// to that slice and the transmittance to it — the same shape as the analytic
// term, except the haze is what the lights actually put in the air along that
// path rather than a constant colour.

// Continuous slice coordinate for a view depth — the inverse of the volume's
// UNIFORM slice mapping, left UNROUNDED so the fetch can interpolate between
// slices instead of snapping to one.
//
// Must agree with `froxel_slice_view_z` in the volumetrics pass; the params it
// reads are copied from that pass's own numbers rather than re-derived,
// because two derivations of the mapping that differ by a hair misregister the
// whole volume.
fn volumetric_slice_coord(view_z: f32) -> f32 {
    let span = max(
        atmosphere_params.froxel_z_far - atmosphere_params.froxel_z_near,
        1e-6,
    );
    return clamp((view_z - atmosphere_params.froxel_z_near) / span, 0.0, 1.0);
}

fn apply_atmosphere_volumetric(
    rgb: vec3<f32>,
    coords: vec2<i32>,
    pixel_center: vec2<f32>,
    screen_dims_f32: vec2<f32>,
    camera: Camera,
) -> vec3<f32> {
    let depth = load_depth(coords);
    let dir = atmosphere_view_ray(pixel_center, screen_dims_f32, camera);
    let cos_theta = atmosphere_forward_cosine(dir, camera);
    let volume_far_z = atmosphere_params.froxel_z_far;

    // Sky sits at the far end of everything: the volume is fully crossed, and
    // the analytic tail runs the saturating distance the analytic path uses.
    var view_z = volume_far_z;
    var dist = ATMOSPHERE_SKY_DISTANCE;
    if (!is_sky_depth(depth)) {
        view_z = linearize_depth(depth, camera);
        dist = view_z / cos_theta;
    }

    // 1. The medium BEYOND the froxel volume, integrated in closed form. Skipped
    //    entirely for anything inside the volume, which is the common case.
    var out_rgb = rgb;
    var tail_transmittance = 1.0;
    let dist_in_volume = min(dist, volume_far_z / cos_theta);
    if (dist > dist_in_volume) {
        // The segment starts where the volume ends, so the height integral has
        // to start from THAT point's altitude rather than the camera's.
        let entry_y = camera.position.y + dir.y * dist_in_volume;
        let tau = atmosphere_optical_depth(
            atmosphere_params,
            entry_y,
            dir.y,
            dist - dist_in_volume,
        );
        let transmittance = exp(-max(tau, 0.0));
        tail_transmittance = transmittance;
        out_rgb = rgb * transmittance + atmosphere_params.color * (1.0 - transmittance);
    }

    // 2. The froxel volume over the near segment.
    //
    // TRILINEAR, not `textureLoad`. The grid is 16 px columns by 32 slices, so
    // a nearest fetch draws every froxel boundary as a step — the beams read as
    // a staircase rather than as air. Sampling in normalized coordinates lets
    // the hardware interpolate across all three axes for one sampler.
    //
    // The half-texel offset matters: a slice's stored value is the accumulation
    // to its FAR face, so the far face of slice i sits at texel i, i.e. at
    // w = (slice_t * n - 0.5) / n. Sampling at slice_t instead shifts the whole
    // volume half a froxel, which shows up as beams leaning away from their
    // light.
    let slice_t = volumetric_slice_coord(view_z);
    let dims = vec3<f32>(textureDimensions(volumetric_tex));
    let uv = pixel_center / screen_dims_f32;
    let w = (slice_t * dims.z - 0.5) / dims.z;
    let uvw = vec3<f32>(uv, clamp(w, 0.0, 1.0));

    let integrated = textureSampleLevel(volumetric_tex, volumetric_sampler, uvw, 0.0);
    // Total scene transmittance = both DISJOINT segments in series: the
    // analytic tail past the volume times the froxel volume's own
    // transmittance. Read by the bloom composite in `main`.
    atmosphere_scene_transmittance = tail_transmittance * integrated.a;
    return out_rgb * integrated.a + integrated.rgb;
}
