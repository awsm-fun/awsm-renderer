fn apply_bloom(
    color: vec3<f32>,
    coords: vec2<i32>,
    screen_dims: vec2<i32>
) -> vec3<f32> {
    // The bloom is pre-built by the dedicated mip-pyramid BloomRenderPass into
    // `bloom_tex` (full-res, intensity already applied). Add it over the
    // INPUT color — not a re-read of `composite_tex`: the input carries the
    // upstream effects chain (SMAA neighborhood blending), and re-reading the
    // composite silently DISCARDED that output whenever bloom was on (the
    // long-standing reason the SMAA toggle appeared to do nothing).
    let bloom = textureLoad(bloom_tex, coords, 0).rgb;
    return color + bloom;
}
