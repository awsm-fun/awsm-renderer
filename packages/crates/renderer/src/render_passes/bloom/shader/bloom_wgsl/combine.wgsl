// Bloom combine — mip-sum upsample into the full-res bloom target.
//
// Sums every pyramid mip via `textureSampleLevel(pyramid, samp, uv, mip)` with
// linear filtering. Coarser mips carry the wide, soft halo; the `scatter`
// param biases how much the coarse (wide) levels contribute. The weighted sum
// is normalized then scaled by `intensity`. This IS the wide glow — sampling
// each mip at the full-res UV upsamples it back with bilinear filtering.

struct BloomParams {
    threshold: f32,
    knee: f32,
    intensity: f32,
    scatter: f32,
};

// Matches `BLOOM_MAX_MIPS` in texture.rs. `textureSampleLevel` clamps the LOD
// to the pyramid's actual `mip_level_count`, so sampling a level the pyramid
// doesn't have simply re-reads the coarsest mip (harmless).
const BLOOM_MAX_MIPS: u32 = 6u;

@group(0) @binding(0) var pyramid: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: BloomParams;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coords = vec2<i32>(gid.xy);
    let dims = textureDimensions(dst);
    if coords.x >= i32(dims.x) || coords.y >= i32(dims.y) {
        return;
    }

    let uv = (vec2<f32>(coords) + vec2<f32>(0.5)) / vec2<f32>(dims);

    // `scatter` biases the coarse mips: scatter == 1 averages all levels
    // evenly (a broad glow); scatter > 1 emphasizes the wide coarse halo;
    // scatter < 1 tightens toward the sharp near-source levels.
    let scatter = max(params.scatter, 1e-4);
    var color = vec3<f32>(0.0);
    var total_weight = 0.0;
    for (var mip = 0u; mip < BLOOM_MAX_MIPS; mip = mip + 1u) {
        let w = pow(scatter, f32(mip));
        color += textureSampleLevel(pyramid, samp, uv, f32(mip)).rgb * w;
        total_weight += w;
    }
    color /= max(total_weight, 1e-5);
    color *= params.intensity;

    textureStore(dst, coords, vec4<f32>(color, 1.0));
}
