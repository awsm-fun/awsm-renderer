// SMAA pass 1 — edge detection (Jimenez et al., luma variant).
//
// Reads the HDR composite, detects luma-contrast edges in compressed,
// GAMMA-ENCODED space (t = (s/(1+s))^(1/2.2)): compressed, so hot emissive
// silhouettes register instead of saturating; encoded, because the reference
// threshold is defined on gamma colour. Applies the reference's local
// contrast adaptation (an edge adjacent to a much stronger edge is spurious
// and would corrupt the pattern search), and writes the RG edges mask:
//   R = edge on the LEFT   border of this pixel (vertical edge line)
//   G = edge on the TOP    border of this pixel (horizontal edge line)

@group(0) @binding(0) var composite_tex: texture_2d<f32>;
@group(0) @binding(1) var edges_tex: texture_storage_2d<rgba8unorm, write>;

// Detection threshold — the reference HIGH-preset default (0.1), applied in
// encoded compressed-luma space, which is comparable to the gamma-LDR range
// the reference assumes. (Compressed but LINEAR is not: there a dark object
// on a dark ground, 0.02 against 0.08, is a delta of 0.06 and no edge, though
// on screen it is a step of about a quarter of the range. Dark scenes got
// almost no anti-aliasing.) Deliberately NOT tuned per-content.
const SMAA_THRESHOLD: f32 = 0.1;
// Local contrast adaptation factor (reference: 2.0) — an edge survives only
// if its delta is at least 1/2 of the strongest neighboring delta.
const SMAA_LOCAL_CONTRAST_ADAPTATION_FACTOR: f32 = 2.0;

fn luma_at(coords: vec2<i32>, dims: vec2<i32>) -> f32 {
    let c = clamp(coords, vec2<i32>(0), dims - vec2<i32>(1));
    let hdr = textureLoad(composite_tex, c, 0).rgb;
    let t = hdr / (vec3<f32>(1.0) + hdr);
    let encoded = pow(max(t, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    return dot(encoded, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims_u = textureDimensions(edges_tex);
    let dims = vec2<i32>(i32(dims_u.x), i32(dims_u.y));
    let coords = vec2<i32>(gid.xy);
    if (coords.x >= dims.x || coords.y >= dims.y) {
        return;
    }

    let l       = luma_at(coords, dims);
    let l_left  = luma_at(coords + vec2<i32>(-1, 0), dims);
    let l_top   = luma_at(coords + vec2<i32>(0, -1), dims);

    let delta_lt = vec2<f32>(abs(l - l_left), abs(l - l_top));
    var edges = step(vec2<f32>(SMAA_THRESHOLD), delta_lt);

    if (edges.x == 0.0 && edges.y == 0.0) {
        textureStore(edges_tex, coords, vec4<f32>(0.0));
        return;
    }

    // Local contrast adaptation: gather the surrounding deltas and suppress
    // edges whose contrast is dominated by a neighbor's.
    let l_right  = luma_at(coords + vec2<i32>(1, 0), dims);
    let l_bottom = luma_at(coords + vec2<i32>(0, 1), dims);
    let delta_r = abs(l - l_right);
    let delta_b = abs(l - l_bottom);

    let l_leftleft = luma_at(coords + vec2<i32>(-2, 0), dims);
    let l_toptop   = luma_at(coords + vec2<i32>(0, -2), dims);
    let delta_ll = abs(l_left - l_leftleft);
    let delta_tt = abs(l_top - l_toptop);

    let max_delta = max(
        max(max(delta_lt.x, delta_lt.y), max(delta_r, delta_b)),
        max(delta_ll, delta_tt),
    );
    edges *= step(
        vec2<f32>(max_delta),
        SMAA_LOCAL_CONTRAST_ADAPTATION_FACTOR * delta_lt,
    );

    textureStore(edges_tex, coords, vec4<f32>(edges, 0.0, 0.0));
}
