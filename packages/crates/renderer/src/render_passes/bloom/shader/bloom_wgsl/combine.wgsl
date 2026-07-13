// Bloom combine — writes the accumulated up-pyramid into the full-res bloom
// target.
//
// The progressive upsample chain (upsample.wgsl) has already tent-filtered
// and scatter-weight-summed every pyramid level into up-pyramid mip 0, so
// this step is a single tent9 tap of that mip at the full-res UV (the final
// half → full ×2 upsample), normalized by the total mip weight
// Σ_{k<n} scatter^k, times `intensity`. The old single-pass mip-sum here
// (one bilinear tap per mip at full-res UV) is what produced boxy quads and
// discrete halo rings from the coarse mips.
//
// `pyramid` is the UP pyramid's all-mips view when mip_count > 1 — only
// mip 0 is sampled, but the all-mips view keeps `textureNumLevels` == the
// number of contributing pyramid levels for the normalization — and the DOWN
// pyramid (mip_count == 1: nothing to upsample, mip 0 already is the sum).

struct BloomParams {
    threshold: f32,
    knee: f32,
    intensity: f32,
    scatter: f32,
};

@group(0) @binding(0) var pyramid: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: BloomParams;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

// 3×3 tent filter of pyramid mip 0 with offsets of one mip-0 texel around
// `uv`. Weights (1 2 1 / 2 4 2 / 1 2 1) / 16 — the taps sum to exactly the
// 16 divisor (4·1 + 4·2 + 1·4), so the kernel is energy-preserving. Pinned
// by `bloom_shaders_validate` in wgsl_validation.rs.
fn tent9(uv: vec2<f32>, texel: vec2<f32>) -> vec3<f32> {
    var c = textureSampleLevel(pyramid, samp, uv, 0.0).rgb * 4.0;
    c += textureSampleLevel(pyramid, samp, uv + vec2<f32>(-texel.x, 0.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(pyramid, samp, uv + vec2<f32>( texel.x, 0.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(pyramid, samp, uv + vec2<f32>(0.0, -texel.y), 0.0).rgb * 2.0;
    c += textureSampleLevel(pyramid, samp, uv + vec2<f32>(0.0,  texel.y), 0.0).rgb * 2.0;
    c += textureSampleLevel(pyramid, samp, uv + vec2<f32>(-texel.x, -texel.y), 0.0).rgb;
    c += textureSampleLevel(pyramid, samp, uv + vec2<f32>( texel.x, -texel.y), 0.0).rgb;
    c += textureSampleLevel(pyramid, samp, uv + vec2<f32>(-texel.x,  texel.y), 0.0).rgb;
    c += textureSampleLevel(pyramid, samp, uv + vec2<f32>( texel.x,  texel.y), 0.0).rgb;
    return c * (1.0 / 16.0);
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coords = vec2<i32>(gid.xy);
    let dims = textureDimensions(dst);
    if coords.x >= i32(dims.x) || coords.y >= i32(dims.y) {
        return;
    }

    let uv = (vec2<f32>(coords) + vec2<f32>(0.5)) / vec2<f32>(dims);
    let texel = vec2<f32>(1.0) / vec2<f32>(textureDimensions(pyramid, 0));
    var color = tent9(uv, texel);

    // Normalize by the total accumulated weight Σ_{k=0}^{n-1} scatter^k so
    // `scatter` keeps its pre-progressive-upsample semantics: the relative
    // weight of pyramid level k is scatter^k — 1 averages all levels evenly
    // (a broad glow), > 1 emphasizes the wide coarse halo, < 1 tightens
    // toward the sharp near-source levels.
    let scatter = max(params.scatter, 0.0);
    let n = textureNumLevels(pyramid);
    var total_weight = 0.0;
    var w = 1.0;
    for (var k = 0u; k < n; k = k + 1u) {
        total_weight += w;
        w *= scatter;
    }
    color /= max(total_weight, 1e-5);
    color *= params.intensity;

    textureStore(dst, coords, vec4<f32>(color, 1.0));
}
