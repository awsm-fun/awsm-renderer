// Bloom effect configuration
const BLOOM_THRESHOLD: f32 = 0.8;
const BLOOM_INTENSITY: f32 = 0.5;
const BLOOM_RADIUS: f32 = 2.0;

fn bloom_threshold(color: vec3<f32>) -> vec3<f32> {
    let brightness = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let contribution = max(brightness - BLOOM_THRESHOLD, 0.0);
    // Soft knee for smoother threshold transition
    let soft_threshold = BLOOM_THRESHOLD * 0.8;
    let knee = BLOOM_THRESHOLD - soft_threshold;
    let soft = clamp((brightness - soft_threshold) / knee, 0.0, 1.0);
    let factor = contribution / max(brightness, 0.0001) * soft;
    return color * factor;
}

fn gaussian_weight(dist_sq: f32, sigma: f32) -> f32 {
    return exp(-dist_sq / (2.0 * sigma * sigma));
}

// 13-tap blur using a tent/bilinear-friendly pattern
fn blur_sample(
    source_tex: texture_2d<f32>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>
) -> vec3<f32> {
    let sigma = BLOOM_RADIUS;
    let r = i32(ceil(BLOOM_RADIUS));

    var result = vec3<f32>(0.0);
    var total_weight = 0.0;

    // Sample in a plus pattern with varying distances for smooth falloff
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let offset = vec2<i32>(dx, dy);
            let dist_sq = f32(dx * dx + dy * dy);

            // Skip corners beyond radius for circular kernel
            if (dist_sq > BLOOM_RADIUS * BLOOM_RADIUS + 0.5) {
                continue;
            }

            let sample_coords = clamp(coords + offset, vec2<i32>(0), screen_dims - 1);
            let weight = gaussian_weight(dist_sq, sigma);

            result += textureLoad(source_tex, sample_coords, 0).rgb * weight;
            total_weight += weight;
        }
    }

    return result / total_weight;
}

{% if bloom_extract %}
fn apply_bloom(
    color: vec3<f32>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>
) -> vec3<f32> {
    let sigma = BLOOM_RADIUS;
    let r = i32(ceil(BLOOM_RADIUS));

    var bloom_color = vec3<f32>(0.0);
    var total_weight = 0.0;

    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let dist_sq = f32(dx * dx + dy * dy);
            if (dist_sq > BLOOM_RADIUS * BLOOM_RADIUS + 0.5) {
                continue;
            }

            let sample_coords = clamp(coords + vec2<i32>(dx, dy), vec2<i32>(0), screen_dims - 1);
            let weight = gaussian_weight(dist_sq, sigma);

            let sample_color = textureLoad(composite_tex, sample_coords, 0).rgb;
            bloom_color += bloom_threshold(sample_color) * weight;
            total_weight += weight;
        }
    }

    return bloom_color / total_weight;
}
{% elif bloom_blend %}
fn apply_bloom(
    color: vec3<f32>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>
) -> vec3<f32> {
    {% if !ping_pong %}
    let blurred = blur_sample(bloom_tex, coords, screen_dims);
    {% else %}
    let blurred = blur_sample(effects_tex, coords, screen_dims);
    {% endif %}

    let original = textureLoad(composite_tex, coords, 0).rgb;
    return original + blurred * BLOOM_INTENSITY;
}
{% else %}
fn apply_bloom(
    color: vec3<f32>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>
) -> vec3<f32> {
    {% if !ping_pong %}
    return blur_sample(bloom_tex, coords, screen_dims);
    {% else %}
    return blur_sample(effects_tex, coords, screen_dims);
    {% endif %}
}
{% endif %}
