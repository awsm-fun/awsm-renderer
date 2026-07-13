// Bloom downsample — COD/Jimenez 13-tap ×2 downsample.
//
// Two variants driven by the `prefilter` askama flag:
// - prefilter: reads the full-res HDR composite and applies a Karis/COD
//   soft-knee threshold before writing pyramid mip 0 (half-res).
// - plain: 13-tap downsample of the previous pyramid mip into the next.
//
// Both sample `src_tex` through a linear sampler at the DESTINATION texel's
// UV, so the 13-tap footprint straddles the 2× larger source correctly.

struct BloomParams {
    threshold: f32,
    knee: f32,
    intensity: f32,
    scatter: f32,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: BloomParams;
@group(0) @binding(3) var dst_mip: texture_storage_2d<rgba16float, write>;

{% if prefilter %}
// Karis/COD quadratic soft-knee threshold. `threshold` is the hard cutoff;
// `knee` softens the transition over `[threshold - knee, threshold + knee]`.
fn soft_threshold(color: vec3<f32>) -> vec3<f32> {
    let br = max(color.r, max(color.g, color.b));
    let knee = max(params.knee, 1e-4);
    var soft = clamp(br - params.threshold + knee, 0.0, 2.0 * knee);
    soft = (soft * soft) / (4.0 * knee + 1e-5);
    let contribution = max(soft, br - params.threshold) / max(br, 1e-5);
    return color * contribution;
}
{% endif %}

fn sample_src(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(src_tex, samp, uv, 0.0).rgb;
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_coords = vec2<i32>(gid.xy);
    let dst_dims = textureDimensions(dst_mip);
    if dst_coords.x >= i32(dst_dims.x) || dst_coords.y >= i32(dst_dims.y) {
        return;
    }

    // Destination texel center in [0, 1] UV.
    let uv = (vec2<f32>(dst_coords) + vec2<f32>(0.5)) / vec2<f32>(dst_dims);
    // Offsets are one SOURCE texel; the 13 taps cover the 4×4 source
    // footprint that maps to this destination texel.
    let texel = vec2<f32>(1.0) / vec2<f32>(textureDimensions(src_tex, 0));

    let a = sample_src(uv + texel * vec2<f32>(-2.0, -2.0));
    let b = sample_src(uv + texel * vec2<f32>( 0.0, -2.0));
    let c = sample_src(uv + texel * vec2<f32>( 2.0, -2.0));
    let d = sample_src(uv + texel * vec2<f32>(-2.0,  0.0));
    let e = sample_src(uv + texel * vec2<f32>( 0.0,  0.0));
    let f = sample_src(uv + texel * vec2<f32>( 2.0,  0.0));
    let g = sample_src(uv + texel * vec2<f32>(-2.0,  2.0));
    let h = sample_src(uv + texel * vec2<f32>( 0.0,  2.0));
    let i = sample_src(uv + texel * vec2<f32>( 2.0,  2.0));
    let j = sample_src(uv + texel * vec2<f32>(-1.0, -1.0));
    let k = sample_src(uv + texel * vec2<f32>( 1.0, -1.0));
    let l = sample_src(uv + texel * vec2<f32>(-1.0,  1.0));
    let m = sample_src(uv + texel * vec2<f32>( 1.0,  1.0));

    // COD weights: center 0.125, corners 0.03125, edges 0.0625,
    // inner box 0.125 — sums to 1.0.
    var result = e * 0.125;
    result += (a + c + g + i) * 0.03125;
    result += (b + d + f + h) * 0.0625;
    result += (j + k + l + m) * 0.125;

    {% if prefilter %}
    result = soft_threshold(result);
    {% endif %}

    textureStore(dst_mip, dst_coords, vec4<f32>(result, 1.0));
}
