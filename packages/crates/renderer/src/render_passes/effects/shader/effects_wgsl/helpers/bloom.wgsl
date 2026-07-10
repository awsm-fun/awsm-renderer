fn apply_bloom(
    color: vec3<f32>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>
) -> vec3<f32> {
    // The bloom is pre-built by the dedicated mip-pyramid BloomRenderPass into
    // `bloom_tex` (full-res, intensity already applied). Just add it over the
    // scene — no extra blur, no extra intensity.
    let bloom = textureLoad(bloom_tex, coords, 0).rgb;
    let original = textureLoad(composite_tex, coords, 0).rgb;
    return original + bloom;
}
