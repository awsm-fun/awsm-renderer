// Bloom upsample — progressive 9-tap tent-filter accumulation (Jimenez/CoD,
// as in Unity/UE bloom).
//
// Runs coarsest → finest: for N = mip_count-1 .. 1
//     up[N-1] = down[N-1] + scatter * tent9(src at mip N)
// where `src_coarse` is the DOWN pyramid's coarsest mip on the first step and
// the previously accumulated UP-pyramid mip afterwards. Ping-pong pyramids:
// `rgba16float` has no `read_write` storage access and a mip cannot be both
// sampled and storage-written in one dispatch, so the accumulated chain lives
// in a second texture while the down pyramid supplies the per-level base.
//
// Unrolled, up[0] = Σ_k scatter^k · tent^k(down[k]) — the same per-level
// weighting the old single-pass combine used (w = scatter^mip), but each
// level is now progressively tent-filtered on its way up instead of being
// bilinear-point-sampled once at full res, which removes the boxy quads and
// discrete halo rings from the coarse mips. The combine step normalizes by
// Σ_k scatter^k, so `scatter` keeps its exact old semantics (1 = even mip
// weights; higher emphasizes the wide coarse halo, lower tightens toward the
// sharp near-source levels).

struct BloomParams {
    threshold: f32,
    knee: f32,
    intensity: f32,
    scatter: f32,
};

// Coarser source (mip N) — down pyramid's coarsest on the first step, the
// accumulated up pyramid afterwards. Sampled with the linear clamp sampler.
@group(0) @binding(0) var src_coarse: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: BloomParams;
// Down-pyramid mip N-1 (single-mip view) — the accumulation base, loaded
// texel-exact (same dims as `dst_mip`).
@group(0) @binding(3) var src_prev: texture_2d<f32>;
// Up-pyramid mip N-1 (single-mip view).
@group(0) @binding(4) var dst_mip: texture_storage_2d<rgba16float, write>;

// 3×3 tent filter with offsets of one SOURCE (mip N) texel around `uv`.
// Weights (1 2 1 / 2 4 2 / 1 2 1) / 16 — the taps sum to exactly the 16
// divisor (4·1 + 4·2 + 1·4), so the kernel is energy-preserving. Pinned by
// `bloom_shaders_validate` in wgsl_validation.rs.
fn tent9(uv: vec2<f32>, texel: vec2<f32>) -> vec3<f32> {
    var c = textureSampleLevel(src_coarse, samp, uv, 0.0).rgb * 4.0;
    c += textureSampleLevel(src_coarse, samp, uv + vec2<f32>(-texel.x, 0.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(src_coarse, samp, uv + vec2<f32>( texel.x, 0.0), 0.0).rgb * 2.0;
    c += textureSampleLevel(src_coarse, samp, uv + vec2<f32>(0.0, -texel.y), 0.0).rgb * 2.0;
    c += textureSampleLevel(src_coarse, samp, uv + vec2<f32>(0.0,  texel.y), 0.0).rgb * 2.0;
    c += textureSampleLevel(src_coarse, samp, uv + vec2<f32>(-texel.x, -texel.y), 0.0).rgb;
    c += textureSampleLevel(src_coarse, samp, uv + vec2<f32>( texel.x, -texel.y), 0.0).rgb;
    c += textureSampleLevel(src_coarse, samp, uv + vec2<f32>(-texel.x,  texel.y), 0.0).rgb;
    c += textureSampleLevel(src_coarse, samp, uv + vec2<f32>( texel.x,  texel.y), 0.0).rgb;
    return c * (1.0 / 16.0);
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_coords = vec2<i32>(gid.xy);
    let dst_dims = textureDimensions(dst_mip);
    if dst_coords.x >= i32(dst_dims.x) || dst_coords.y >= i32(dst_dims.y) {
        return;
    }

    // Destination texel center in [0, 1] UV; tent offsets are one COARSE
    // (mip N) texel so the kernel straddles the 2× smaller source correctly.
    let uv = (vec2<f32>(dst_coords) + vec2<f32>(0.5)) / vec2<f32>(dst_dims);
    let texel = vec2<f32>(1.0) / vec2<f32>(textureDimensions(src_coarse, 0));

    let base = textureLoad(src_prev, dst_coords, 0).rgb;
    let scatter = max(params.scatter, 0.0);
    let result = base + tent9(uv, texel) * scatter;
    textureStore(dst_mip, dst_coords, vec4<f32>(result, 1.0));
}
