// Depth of Field.
//
// `linearize_depth` / `load_depth` live in `helpers/depth.wgsl` (shared with
// atmosphere), included by compute.wgsl ahead of this file.

// Depth of Field constants
const DOF_MAX_BLUR: f32 = 20.0;          // Maximum blur radius in pixels
const SENSOR_HEIGHT: f32 = 0.024;        // 24mm full-frame sensor height (in meters)

// Calculate focal length from projection matrix in world units (meters)
// Matches Blender's camera model with 24mm sensor height
fn get_focal_length(camera: Camera) -> f32 {
    // proj[1][1] = 1 / tan(fov_y / 2)
    // focal_length = sensor_height / (2 * tan(fov_y / 2))
    return (SENSOR_HEIGHT * 0.5) * camera.proj[1][1];
}

// Physically-based circle of confusion (Blender-compatible)
// aperture: f-stop number (e.g., 2.8, 5.6, 8.0) - lower = shallower DoF
// focus_distance: distance to focus plane in world units (meters)
fn calculate_coc(linear_depth: f32, camera: Camera) -> f32 {
    let S = camera.focus_distance;        // Focus distance
    let N = camera.aperture;              // F-stop number
    let f = get_focal_length(camera);     // Focal length in world units
    let D = linear_depth;                 // Object distance

    // Aperture diameter
    let A = f / max(N, 0.1);

    // Circle of confusion formula: CoC = A * f * |D - S| / (D * (S - f))
    // For typical distances where S >> f, simplifies to: CoC ≈ A * f * |D - S| / (D * S)
    let coc_world = A * f * abs(D - S) / (D * max(S, 0.001));

    // Convert from world units to pixels
    // Approximate: screen_height_pixels / sensor_height * coc_world
    // Using viewport height from camera, or estimate ~1000px
    let screen_height = camera.viewport_size.y;
    let coc_pixels = coc_world * screen_height / SENSOR_HEIGHT;

    return clamp(coc_pixels, 0.0, DOF_MAX_BLUR);
}

// ── Sampling ────────────────────────────────────────────────────────────────
// Two spirals per pixel, both rotated per pixel so the fixed tap pattern
// turns into fine grain instead of repeating as ghost copies of a highlight:
//
//  SELF  — the pixel's own blur: taps inside ITS circle of confusion. This is
//          what blurs anything behind the focus plane (and the inside of a
//          near object).
//  NEAR  — everything IN FRONT of the focus plane that is blurred ONTO this
//          pixel: taps out to the maximum blur radius, each accepted when
//          that sample's own circle of confusion reaches here
//          (scatter-as-gather).
//
// The NEAR spiral is what a single "blur by my own CoC" gather cannot do: an
// in-focus pixel next to a defocused foreground object has a CoC of zero and
// so never looked at its neighbours — the foreground blur stopped dead at the
// object's silhouette. A bright thin object close to the lens (a brass edge,
// a row of bulbs) therefore stayed a hard line however wide the aperture,
// instead of opening into soft discs.
const DOF_SELF_SAMPLES: u32 = 16u;
const DOF_NEAR_SAMPLES: u32 = 48u;

// Golden-angle spiral with uniform area density over a disc of `radius`,
// rotated by `rotation` radians.
fn get_disk_offset(index: u32, count: u32, radius: f32, rotation: f32) -> vec2<f32> {
    let golden_angle = 2.39996323;
    let theta = f32(index) * golden_angle + rotation;
    let r = sqrt((f32(index) + 0.5) / f32(count)) * radius;
    return vec2<f32>(cos(theta), sin(theta)) * r;
}

// Interleaved gradient noise (Jimenez): a per-pixel value in [0,1) with good
// spatial distribution and no texture.
fn dof_noise(coords: vec2<i32>) -> f32 {
    let p = vec2<f32>(f32(coords.x), f32(coords.y));
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

// One depth sample for a blur tap. The centre pixel uses `load_depth` (nearest
// of the 4 MSAA samples); the taps read sample 0 only — 64 taps × 4 loads
// would be most of the cost of the effect for no visible gain.
fn dof_tap_depth(coords: vec2<i32>) -> f32 {
    // Sample index 0 on a multisampled depth target, mip 0 otherwise — the
    // same call either way.
    return textureLoad(depth_tex, coords, 0);
}

// Apply depth of field blur. `color` is this pixel after the effects that run
// before DoF (haze, bloom); the blur itself gathers the composite.
fn apply_dof(
    color: vec3<f32>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>,
    camera: Camera
) -> vec3<f32> {
    let center_depth = load_depth(coords);
    let center_linear = linearize_depth(center_depth, camera);
    let center_coc = calculate_coc(center_linear, camera);
    let focus = camera.focus_distance;
    let rotation = dof_noise(coords) * 6.2831853;

    // The pre-DoF effects (haze, bloom) were added to this pixel only; the taps
    // below read the raw composite. Carry their contribution across the blur
    // instead of dropping it — otherwise bloom vanished wherever DoF blurred.
    let center_composite = textureLoad(composite_tex, coords, 0).rgb;
    let effects_delta = color - center_composite;

    // ── SELF: this pixel's own blur ──
    var self_color = center_composite;
    if (center_coc >= 0.5) {
        var sum = vec3<f32>(0.0);
        var total = 0.0;
        for (var i = 0u; i < DOF_SELF_SAMPLES; i = i + 1u) {
            let offset = get_disk_offset(i, DOF_SELF_SAMPLES, center_coc, rotation);
            let sample_coords = clamp(
                coords + vec2<i32>(i32(round(offset.x)), i32(round(offset.y))),
                vec2<i32>(0),
                screen_dims - 1
            );
            let sample_color = textureLoad(composite_tex, sample_coords, 0).rgb;
            let sample_linear = linearize_depth(dof_tap_depth(sample_coords), camera);
            let sample_coc = calculate_coc(sample_linear, camera);

            // A sample that is sharper than this pixel and NOT behind it must
            // not smear across it (an in-focus object bleeding into a blurred
            // background); a sample behind this pixel is what its blur reveals.
            var weight = 1.0;
            if (sample_linear < center_linear && sample_coc < center_coc) {
                weight = sample_coc / max(center_coc, 0.01);
            }
            weight = max(weight, 0.02);
            sum += sample_color * weight;
            total += weight;
        }
        let blurred = sum / max(total, 0.01);
        self_color = mix(center_composite, blurred, smoothstep(0.0, 2.0, center_coc));
    }

    // ── NEAR: foreground blur landing on this pixel ──
    var near_sum = vec3<f32>(0.0);
    var near_weight = 0.0;
    for (var i = 0u; i < DOF_NEAR_SAMPLES; i = i + 1u) {
        let offset = get_disk_offset(i, DOF_NEAR_SAMPLES, DOF_MAX_BLUR, rotation);
        let sample_coords = clamp(
            coords + vec2<i32>(i32(round(offset.x)), i32(round(offset.y))),
            vec2<i32>(0),
            screen_dims - 1
        );
        let sample_linear = linearize_depth(dof_tap_depth(sample_coords), camera);
        // Only what is in front of the focus plane spreads forward over
        // sharper things.
        if (sample_linear >= focus) {
            continue;
        }
        let sample_coc = calculate_coc(sample_linear, camera);
        let dist = length(offset);
        // Does that sample's disc reach this pixel? (soft 2px edge)
        let cover = smoothstep(dist - 1.0, dist + 1.0, sample_coc);
        if (cover <= 0.0) {
            continue;
        }
        // Each tap stands for (πR²/N) pixels, each spreading its light over
        // its own disc (π·coc²): weight = (R²/N) / coc².
        let weight = cover * (DOF_MAX_BLUR * DOF_MAX_BLUR)
            / (f32(DOF_NEAR_SAMPLES) * max(sample_coc * sample_coc, 1.0));
        near_sum += textureLoad(composite_tex, sample_coords, 0).rgb * weight;
        near_weight += weight;
    }

    var result = self_color;
    if (near_weight > 0.0) {
        // The foreground layer is laid over the pixel's own result with the
        // coverage it accumulated (1 = fully inside a defocused near object).
        let near_color = near_sum / near_weight;
        result = mix(self_color, near_color, clamp(near_weight, 0.0, 1.0));
    }
    return result + effects_delta;
}
