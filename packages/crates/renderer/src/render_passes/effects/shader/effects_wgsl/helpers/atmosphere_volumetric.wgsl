// Composite the pre-integrated froxel volume over the scene.
//
// All the work happened in the volumetrics pass; this is one fetch. The volume
// stores, per froxel, the in-scatter accumulated from the eye to that slice and
// the transmittance to it, so the composite is
//
//     rgb * transmittance + accumulated_in_scatter
//
// which is the same shape as the analytic `rgb * t + haze * (1 - t)` — except
// the haze term is what the lights actually put in the air along that path
// rather than a constant colour. That's the entire difference between the two
// modes at this point in the frame.

// Continuous slice coordinate for a view depth — the inverse of the volume's
// exponential slice mapping, left UNROUNDED so the fetch can interpolate
// between slices instead of snapping to one.
//
// Must agree with `froxel_slice_view_z` in the volumetrics pass; the params it
// reads are copied from that pass's own numbers rather than re-derived, because
// two derivations of an exponential mapping that differ by a hair misregister
// the whole volume.
fn volumetric_slice_coord(view_z: f32) -> f32 {
    let z = max(view_z, atmosphere_params.froxel_z_near);
    let t = log(z / atmosphere_params.froxel_z_near)
        / max(atmosphere_params.froxel_log_far_over_near, 1e-6);
    return clamp(t, 0.0, 1.0);
}

fn apply_atmosphere_volumetric(
    rgb: vec3<f32>,
    coords: vec2<i32>,
    pixel_center: vec2<f32>,
    screen_dims_f32: vec2<f32>,
    camera: Camera,
) -> vec3<f32> {
    let depth = load_depth(coords);
    // Sky takes the far end: the volume stops where the froxel grid stops, and
    // anything beyond has already crossed all the air the volume describes.
    var slice_t = 1.0;
    if (!is_sky_depth(depth)) {
        slice_t = volumetric_slice_coord(linearize_depth(depth, camera));
    }

    // TRILINEAR, not `textureLoad`. The grid is 16 px columns by 32 slices, so
    // a nearest fetch draws every froxel boundary as a step — the beams read as
    // a staircase rather than as air. Sampling in normalized coordinates lets
    // the hardware interpolate across all three axes for one sampler.
    //
    // The half-texel offset matters: froxel values live at CENTRES, so the UV
    // for column i is (i + 0.5)/n. Sampling at i/n instead shifts the whole
    // volume half a froxel toward the origin, which shows up as beams leaning
    // away from their light.
    let dims = vec3<f32>(textureDimensions(volumetric_tex));
    let uv = pixel_center / screen_dims_f32;
    let w = (slice_t * dims.z - 0.5) / dims.z;
    let uvw = vec3<f32>(uv, clamp(w, 0.0, 1.0));

    let integrated = textureSampleLevel(volumetric_tex, volumetric_sampler, uvw, 0.0);
    return rgb * integrated.a + integrated.rgb;
}
