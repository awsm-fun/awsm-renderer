// Atmospheric haze — a stylized exponential medium, integrated analytically
// along the view ray. No marching: the height profile has a closed form, so
// this is a handful of ALU per pixel on top of the depth read DoF already does.
//
// The medium itself — the density profile, its closed-form integral, and the
// view-ray reconstruction — lives in `atmosphere_medium.wgsl`, which renders
// ahead of this file and is shared with the volumetric path. What's left here
// is only this phase's application of it: integrate the WHOLE view ray and
// blend toward the haze colour.

// Blend `rgb` toward the haze colour by how much medium the pixel's ray
// crossed. Applied BEFORE bloom so the glow blooms the hazed image, not the
// clear one — haze that sits on top of bloom reads as a flat wash over the
// lights rather than as air between them.
fn apply_atmosphere(
    rgb: vec3<f32>,
    coords: vec2<i32>,
    pixel_center: vec2<f32>,
    screen_dims_f32: vec2<f32>,
    camera: Camera,
) -> vec3<f32> {
    let params = atmosphere_params;
    let depth = load_depth(coords);
    let dir = atmosphere_view_ray(pixel_center, screen_dims_f32, camera);

    var dist: f32;
    if (is_sky_depth(depth)) {
        dist = ATMOSPHERE_SKY_DISTANCE;
    } else {
        // `linearize_depth` returns distance along the camera's forward axis;
        // the ray travels further than that off-axis.
        dist = linearize_depth(depth, camera) / atmosphere_forward_cosine(dir, camera);
    }

    let tau = atmosphere_optical_depth(params, camera.position.y, dir.y, dist);
    let transmittance = exp(-max(tau, 0.0));
    return rgb * transmittance + params.color * (1.0 - transmittance);
}
