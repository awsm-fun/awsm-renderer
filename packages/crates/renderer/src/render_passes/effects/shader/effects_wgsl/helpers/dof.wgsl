// Depth of Field constants
const DOF_MAX_BLUR: f32 = 16.0;          // Maximum blur radius in pixels
const DOF_SAMPLES: u32 = 16u;            // Number of samples for blur disk
const SENSOR_HEIGHT: f32 = 0.024;        // 24mm full-frame sensor height (in meters)

// Linearize depth from NDC depth buffer value
fn linearize_depth(depth: f32, camera: Camera) -> f32 {
    let near = camera.proj[3][2];

    // Check for reverse-Z infinite far (proj[2][2] ≈ 0)
    if (abs(camera.proj[2][2]) < 0.0001) {
        // Reverse-Z with infinite far: depth = near / z, so z = near / depth
        return near / max(depth, 0.0001);
    } else {
        // Standard depth buffer
        let far = camera.proj[3][2] / (camera.proj[2][2] + 1.0);
        return (near * far) / (far - depth * (far - near));
    }
}

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

// Load depth, handling both multisampled and single-sampled textures
fn load_depth(coords: vec2<i32>) -> f32 {
    {% if multisampled_geometry %}
        var min_depth = 1.0;
        for (var s = 0u; s < 4u; s = s + 1u) {
            let d = textureLoad(depth_tex, coords, i32(s));
            min_depth = min(min_depth, d);
        }
        return min_depth;
    {% else %}
        return textureLoad(depth_tex, coords, 0);
    {% endif %}
}

// Disk sample offsets using golden angle distribution
fn get_disk_offset(index: u32, radius: f32) -> vec2<f32> {
    let golden_angle = 2.39996323;
    let theta = f32(index) * golden_angle;
    let r = sqrt(f32(index + 1u) / f32(DOF_SAMPLES)) * radius;
    return vec2<f32>(cos(theta), sin(theta)) * r;
}

// Apply depth of field blur
fn apply_dof(
    color: vec3<f32>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>,
    camera: Camera
) -> vec3<f32> {
    let center_depth = load_depth(coords);
    let center_linear = linearize_depth(center_depth, camera);
    let center_coc = calculate_coc(center_linear, camera);

    // No blur needed if CoC is very small
    if (center_coc < 0.5) {
        return color;
    }

    var blur_color = vec3<f32>(0.0);
    var total_weight = 0.0;

    for (var i = 0u; i < DOF_SAMPLES; i = i + 1u) {
        let offset = get_disk_offset(i, center_coc);
        let sample_coords = clamp(
            coords + vec2<i32>(i32(round(offset.x)), i32(round(offset.y))),
            vec2<i32>(0),
            screen_dims - 1
        );

        let sample_color = textureLoad(composite_tex, sample_coords, 0).rgb;
        let sample_depth = load_depth(sample_coords);
        let sample_linear = linearize_depth(sample_depth, camera);
        let sample_coc = calculate_coc(sample_linear, camera);

        // Prevent background from bleeding into foreground
        var weight = 1.0;
        if (sample_linear > center_linear && sample_coc < center_coc) {
            weight = sample_coc / max(center_coc, 0.01);
        }

        let dist = length(offset);
        weight *= 1.0 - smoothstep(center_coc * 0.5, center_coc, dist);
        weight = max(weight, 0.01);

        blur_color += sample_color * weight;
        total_weight += weight;
    }

    blur_color = blur_color / max(total_weight, 0.01);

    let blend_factor = smoothstep(0.0, 2.0, center_coc);
    return mix(color, blur_color, blend_factor);
}
