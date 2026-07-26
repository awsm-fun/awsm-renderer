// Atmospheric haze — a stylized exponential medium, integrated analytically
// along the view ray. No marching: the height profile has a closed form, so
// this is a handful of ALU per pixel on top of the depth read DoF already does.
//
// The medium's density at world height y is
//
//     rho(y) = density * exp(-height_falloff * max(y - base_height, 0))
//
// i.e. full below `base_height` and thinning exponentially above it. That
// clamp is what makes haze POOL — an unclamped exp would blow up downward and
// swallow anything below the base plane.
//
// Optical depth over a ray segment is then the integral of rho along it. With
// h = y - base_height and G'(h) = exp(-falloff * max(h, 0)):
//
//     G(h) = h                          for h <= 0     (full density)
//     G(h) = (1 - exp(-falloff*h)) / k   for h  > 0     (thinning)
//
// which is continuous at 0 (G(0) = 0) and gives
//
//     tau = density * (G(h_end) - G(h_start)) / dir.y
//
// Looking UP through a thinning medium, G converges to 1/falloff, so the sky
// keeps a finite haze amount however far the ray goes. Looking along the
// horizon it saturates — which is the horizon band you actually want.
//
// PERSPECTIVE CAMERA ASSUMPTION: the view ray is reconstructed through
// `inv_proj` from the pixel, so an orthographic projection would get the wrong
// ray origin. Same assumption DoF makes two files over; if ortho ever matters
// here, both need the fix together.

// `AtmosphereParamsRaw` + the `atmosphere_params` binding are declared in
// `bind_groups.wgsl`, which renders ahead of this file.

// How far a sky pixel's ray is treated as travelling. Large enough that a
// uniform medium saturates completely (exp(-density * 1e5) underflows for any
// usable density), while a height-thinned one still converges via G().
const ATMOSPHERE_SKY_DISTANCE: f32 = 1.0e5;
// Below this |dir.y| the ray is horizontal enough that dividing by it loses
// all precision; the medium is effectively constant along it, so integrate at
// the start height instead.
const ATMOSPHERE_MIN_DIR_Y: f32 = 1.0e-4;

// Antiderivative of exp(-k * max(h, 0)), continuous at h = 0.
fn atmosphere_height_integral(h: f32, k: f32) -> f32 {
    if (h <= 0.0) {
        return h;
    }
    return (1.0 - exp(-k * h)) / k;
}

// Optical depth over `dist` metres of medium along a ray starting at
// `start_y` and travelling with vertical component `dir_y`.
fn atmosphere_optical_depth(
    params: AtmosphereParamsRaw,
    start_y: f32,
    dir_y: f32,
    dist: f32,
) -> f32 {
    let k = params.height_falloff;
    if (k <= 0.0) {
        // Uniform medium — no height structure to integrate.
        return params.density * dist;
    }

    let h_start = start_y - params.base_height;

    if (abs(dir_y) < ATMOSPHERE_MIN_DIR_Y) {
        // Effectively horizontal: constant density over the whole segment.
        return params.density * dist * exp(-k * max(h_start, 0.0));
    }

    let h_end = h_start + dir_y * dist;
    let integral = atmosphere_height_integral(h_end, k)
        - atmosphere_height_integral(h_start, k);
    // The sign of `dir_y` cancels between numerator and denominator, so this
    // is positive for rays going either up or down.
    return params.density * integral / dir_y;
}

// World-space direction of the view ray through `pixel_center`.
fn atmosphere_view_ray(
    pixel_center: vec2<f32>,
    screen_dims_f32: vec2<f32>,
    camera: Camera,
) -> vec3<f32> {
    // Pixel → NDC. Y flips: NDC is +up, texel coords are +down.
    let uv = pixel_center / screen_dims_f32;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    // Any depth gives the same ray direction under a perspective projection.
    let view_h = camera.inv_proj * vec4<f32>(ndc.x, ndc.y, 1.0, 1.0);
    let view_dir = normalize(view_h.xyz / view_h.w);
    return normalize((camera.inv_view * vec4<f32>(view_dir, 0.0)).xyz);
}

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
        // the ray travels further than that off-axis. `view_dir.z` is the
        // forward cosine (view space looks down -Z), recovered here as the
        // dot of the world ray with the camera's forward vector.
        let forward = normalize(-(camera.inv_view * vec4<f32>(0.0, 0.0, 1.0, 0.0)).xyz);
        let cos_theta = max(dot(dir, forward), ATMOSPHERE_MIN_DIR_Y);
        dist = linearize_depth(depth, camera) / cos_theta;
    }

    let tau = atmosphere_optical_depth(params, camera.position.y, dir.y, dist);
    let transmittance = exp(-max(tau, 0.0));
    return rgb * transmittance + params.color * (1.0 - transmittance);
}
