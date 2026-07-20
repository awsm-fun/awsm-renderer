// SMAA pass 2 — blend-weight calculation. Faithful WGSL port of the REFERENCE
// implementation (Jimenez et al., SMAA.hlsl `SMAABlendingWeightCalculationPS`,
// SMAA 1x, preset HIGH), including:
//   • diagonal pattern detection (`SMAACalculateDiagWeights`),
//   • SearchTex-accelerated orthogonal edge searches,
//   • AreaTex pattern-area lookups (the genuine 160×560 RG8 texture),
//   • corner rounding.
// Every `SampleLevelZero` becomes `textureSampleLevel(..., 0.0)` with a linear
// clamp sampler — SMAA needs no derivatives, so the compute context is exact.
//
// Output layout matches the reference blend texture:
//   RG = horizontal-edge blend weights, BA = vertical-edge blend weights,
// consumed by the reference neighborhood-blending port in the effects pass.

@group(0) @binding(0) var edges_tex: texture_2d<f32>;
@group(0) @binding(1) var linear_samp: sampler;
@group(0) @binding(2) var area_tex: texture_2d<f32>;
@group(0) @binding(3) var search_tex: texture_2d<f32>;
@group(0) @binding(4) var weights_tex: texture_storage_2d<rgba8unorm, write>;

// Preset HIGH.
const SMAA_MAX_SEARCH_STEPS: f32 = 16.0;
const SMAA_MAX_SEARCH_STEPS_DIAG: f32 = 8.0;
const SMAA_CORNER_ROUNDING: f32 = 25.0;

const SMAA_AREATEX_MAX_DISTANCE: f32 = 16.0;
const SMAA_AREATEX_MAX_DISTANCE_DIAG: f32 = 20.0;
const SMAA_AREATEX_PIXEL_SIZE: vec2<f32> = vec2<f32>(1.0 / 160.0, 1.0 / 560.0);
const SMAA_AREATEX_SUBTEX_SIZE: f32 = 1.0 / 7.0;
const SMAA_SEARCHTEX_SIZE: vec2<f32> = vec2<f32>(66.0, 33.0);
const SMAA_SEARCHTEX_PACKED_SIZE: vec2<f32> = vec2<f32>(64.0, 16.0);
const SMAA_CORNER_ROUNDING_NORM: f32 = SMAA_CORNER_ROUNDING / 100.0;

var<private> rt_metrics: vec4<f32>;   // (1/w, 1/h, w, h) — SMAA_RT_METRICS

fn sample_edges(coord: vec2<f32>) -> vec2<f32> {
    return textureSampleLevel(edges_tex, linear_samp, coord, 0.0).rg;
}
fn sample_area(coord: vec2<f32>) -> vec2<f32> {
    return textureSampleLevel(area_tex, linear_samp, coord, 0.0).rg;
}
fn sample_search(coord: vec2<f32>) -> f32 {
    return textureSampleLevel(search_tex, linear_samp, coord, 0.0).r;
}

// ─── Diagonal pattern functions (reference: SMAA*Diag*) ────────────────────

fn search_diag1(texcoord: vec2<f32>, dir: vec2<f32>, e_out: ptr<function, vec2<f32>>) -> vec2<f32> {
    var coord = vec4<f32>(texcoord, -1.0, 1.0);
    let t = vec3<f32>(rt_metrics.x, rt_metrics.y, 1.0);
    loop {
        if (!(coord.z < SMAA_MAX_SEARCH_STEPS_DIAG - 1.0 && coord.w > 0.9)) { break; }
        let adv = t * vec3<f32>(dir, 1.0) + vec3<f32>(coord.x, coord.y, coord.z);
        let e = sample_edges(vec2<f32>(adv.x, adv.y));
        coord = vec4<f32>(adv.x, adv.y, adv.z, dot(e, vec2<f32>(0.5, 0.5)));
        *e_out = e;
    }
    return vec2<f32>(coord.z, coord.w);
}

fn decode_diag_bilinear_access_2(e_in: vec2<f32>) -> vec2<f32> {
    var e = e_in;
    e.r = e.r * abs(5.0 * e.r - 5.0 * 0.75);
    return round(e);
}

fn decode_diag_bilinear_access_4(e_in: vec4<f32>) -> vec4<f32> {
    let er = vec2<f32>(e_in.r, e_in.b) * abs(5.0 * vec2<f32>(e_in.r, e_in.b) - vec2<f32>(3.75));
    return round(vec4<f32>(er.x, e_in.g, er.y, e_in.a));
}

fn search_diag2(texcoord: vec2<f32>, dir: vec2<f32>, e_out: ptr<function, vec2<f32>>) -> vec2<f32> {
    var coord = vec4<f32>(texcoord, -1.0, 1.0);
    coord.x += 0.25 * rt_metrics.x;
    let t = vec3<f32>(rt_metrics.x, rt_metrics.y, 1.0);
    loop {
        if (!(coord.z < SMAA_MAX_SEARCH_STEPS_DIAG - 1.0 && coord.w > 0.9)) { break; }
        let adv = t * vec3<f32>(dir, 1.0) + vec3<f32>(coord.x, coord.y, coord.z);
        var e = sample_edges(vec2<f32>(adv.x, adv.y));
        e = decode_diag_bilinear_access_2(e);
        coord = vec4<f32>(adv.x, adv.y, adv.z, dot(e, vec2<f32>(0.5, 0.5)));
        *e_out = e;
    }
    return vec2<f32>(coord.z, coord.w);
}

fn area_diag(dist: vec2<f32>, e: vec2<f32>, offset: f32) -> vec2<f32> {
    var texcoord = vec2<f32>(SMAA_AREATEX_MAX_DISTANCE_DIAG) * e + dist;
    texcoord = SMAA_AREATEX_PIXEL_SIZE * texcoord + 0.5 * SMAA_AREATEX_PIXEL_SIZE;
    texcoord.x += 0.5;
    texcoord.y += SMAA_AREATEX_SUBTEX_SIZE * offset;
    return sample_area(texcoord);
}

fn calculate_diag_weights(texcoord: vec2<f32>, e: vec2<f32>, subsample_indices: vec4<f32>) -> vec2<f32> {
    var weights = vec2<f32>(0.0);
    var d = vec4<f32>(0.0);

    // ── First diagonal (↗ line: edges going down-left / up-right) ──
    var end1 = vec2<f32>(0.0);
    if (e.r > 0.0) {
        let dxz = search_diag1(texcoord, vec2<f32>(-1.0, 1.0), &end1);
        d.x = dxz.x + f32(end1.y > 0.9);
        d.z = dxz.y;
    }
    var end2 = vec2<f32>(0.0);
    let dyw = search_diag1(texcoord, vec2<f32>(1.0, -1.0), &end2);
    d.y = dyw.x;
    d.w = dyw.y;

    if (d.x + d.y > 2.0) {
        let coords = vec4<f32>(-d.x + 0.25, d.x, d.y, -d.y - 0.25) * rt_metrics.xyxy
            + vec4<f32>(texcoord, texcoord);
        let c_xy = sample_edges(vec2<f32>(coords.x - rt_metrics.x, coords.y));
        let c_zw = sample_edges(vec2<f32>(coords.z + rt_metrics.x, coords.w));
        // Reference: c.yxwz = SMAADecodeDiagBilinearAccess(c.xyzw), then
        // cc = 2*c.xz + c.yw — i.e. cc = 2*(dec.y, dec.w) + (dec.x, dec.z).
        let dec = decode_diag_bilinear_access_4(vec4<f32>(c_xy.x, c_xy.y, c_zw.x, c_zw.y));
        var cc = vec2<f32>(2.0) * vec2<f32>(dec.y, dec.w) + vec2<f32>(dec.x, dec.z);

        // Remove the crossing edge if we didn't find the end of the line:
        cc = cc * step(vec2<f32>(d.z, d.w), vec2<f32>(0.9));

        weights += area_diag(vec2<f32>(d.x, d.y), cc, subsample_indices.z);
    }

    // ── Second diagonal (↘ line) ──
    var end3 = vec2<f32>(0.0);
    let dxz2 = search_diag2(texcoord, vec2<f32>(-1.0, -1.0), &end3);
    d.x = dxz2.x;
    d.z = dxz2.y;
    let e_right = sample_edges(texcoord + vec2<f32>(rt_metrics.x, 0.0)).r;
    if (e_right > 0.0) {
        var end4 = vec2<f32>(0.0);
        let dyw2 = search_diag2(texcoord, vec2<f32>(1.0, 1.0), &end4);
        d.y = dyw2.x + f32(end4.y > 0.9);
        d.w = dyw2.y;
    } else {
        d.y = 0.0;
        d.w = 0.0;
    }

    if (d.x + d.y > 2.0) {
        let coords = vec4<f32>(-d.x, -d.x, d.y, d.y) * rt_metrics.xyxy
            + vec4<f32>(texcoord, texcoord);
        var c = vec4<f32>(0.0);
        c.x = sample_edges(vec2<f32>(coords.x - rt_metrics.x, coords.y)).g;
        c.y = sample_edges(vec2<f32>(coords.x, coords.y - rt_metrics.y)).r;
        let c_zw = sample_edges(vec2<f32>(coords.z + rt_metrics.x, coords.w));
        c.z = c_zw.g;
        c.w = c_zw.r;
        var cc = vec2<f32>(2.0) * vec2<f32>(c.x, c.z) + vec2<f32>(c.y, c.w);
        cc = cc * step(vec2<f32>(d.z, d.w), vec2<f32>(0.9));

        let w2 = area_diag(vec2<f32>(d.x, d.y), cc, subsample_indices.w);
        weights += vec2<f32>(w2.y, w2.x);
    }

    return weights;
}

// ─── Orthogonal search functions (reference: SMAASearch*) ──────────────────

fn search_length(e: vec2<f32>, offset: f32) -> f32 {
    var scale = SMAA_SEARCHTEX_SIZE * vec2<f32>(0.5, -1.0);
    var bias = SMAA_SEARCHTEX_SIZE * vec2<f32>(offset, 1.0);
    scale += vec2<f32>(-1.0, 1.0);
    bias += vec2<f32>(0.5, -0.5);
    scale *= vec2<f32>(1.0) / SMAA_SEARCHTEX_PACKED_SIZE;
    bias *= vec2<f32>(1.0) / SMAA_SEARCHTEX_PACKED_SIZE;
    return sample_search(scale * e + bias);
}

fn search_x_left(texcoord_in: vec2<f32>, end: f32) -> f32 {
    var texcoord = texcoord_in;
    var e = vec2<f32>(0.0, 1.0);
    loop {
        if (!(texcoord.x > end && e.g > 0.8281 && e.r == 0.0)) { break; }
        e = sample_edges(texcoord);
        texcoord = vec2<f32>(-2.0, 0.0) * rt_metrics.xy + texcoord;
    }
    let offset = 3.25 - (255.0 / 127.0) * search_length(e, 0.0);
    return rt_metrics.x * offset + texcoord.x;
}

fn search_x_right(texcoord_in: vec2<f32>, end: f32) -> f32 {
    var texcoord = texcoord_in;
    var e = vec2<f32>(0.0, 1.0);
    loop {
        if (!(texcoord.x < end && e.g > 0.8281 && e.r == 0.0)) { break; }
        e = sample_edges(texcoord);
        texcoord = vec2<f32>(2.0, 0.0) * rt_metrics.xy + texcoord;
    }
    let offset = 3.25 - (255.0 / 127.0) * search_length(e, 0.5);
    return -rt_metrics.x * offset + texcoord.x;
}

fn search_y_up(texcoord_in: vec2<f32>, end: f32) -> f32 {
    var texcoord = texcoord_in;
    var e = vec2<f32>(1.0, 0.0);
    loop {
        if (!(texcoord.y > end && e.r > 0.8281 && e.g == 0.0)) { break; }
        e = sample_edges(texcoord);
        texcoord = vec2<f32>(0.0, -2.0) * rt_metrics.xy + texcoord;
    }
    let offset = 3.25 - (255.0 / 127.0) * search_length(vec2<f32>(e.g, e.r), 0.0);
    return rt_metrics.y * offset + texcoord.y;
}

fn search_y_down(texcoord_in: vec2<f32>, end: f32) -> f32 {
    var texcoord = texcoord_in;
    var e = vec2<f32>(1.0, 0.0);
    loop {
        if (!(texcoord.y < end && e.r > 0.8281 && e.g == 0.0)) { break; }
        e = sample_edges(texcoord);
        texcoord = vec2<f32>(0.0, 2.0) * rt_metrics.xy + texcoord;
    }
    let offset = 3.25 - (255.0 / 127.0) * search_length(vec2<f32>(e.g, e.r), 0.5);
    return -rt_metrics.y * offset + texcoord.y;
}

fn area_ortho(dist: vec2<f32>, e1: f32, e2: f32, offset: f32) -> vec2<f32> {
    var texcoord = vec2<f32>(SMAA_AREATEX_MAX_DISTANCE) * round(4.0 * vec2<f32>(e1, e2)) + dist;
    texcoord = SMAA_AREATEX_PIXEL_SIZE * texcoord + 0.5 * SMAA_AREATEX_PIXEL_SIZE;
    texcoord.y += SMAA_AREATEX_SUBTEX_SIZE * offset;
    return sample_area(texcoord);
}

// ─── Corner rounding (reference: SMAADetect*CornerPattern) ─────────────────

fn detect_horizontal_corner_pattern(weights_in: vec2<f32>, texcoord: vec4<f32>, d: vec2<f32>) -> vec2<f32> {
    let left_right = step(d, vec2<f32>(d.y, d.x));
    var rounding = (1.0 - SMAA_CORNER_ROUNDING_NORM) * left_right;
    rounding = rounding / max(left_right.x + left_right.y, 1e-5);
    var factor = vec2<f32>(1.0);
    factor.x -= rounding.x * sample_edges(vec2<f32>(texcoord.x, texcoord.y) + vec2<f32>(0.0, rt_metrics.y)).r;
    factor.x -= rounding.y * sample_edges(vec2<f32>(texcoord.z, texcoord.w) + vec2<f32>(rt_metrics.x, rt_metrics.y)).r;
    factor.y -= rounding.x * sample_edges(vec2<f32>(texcoord.x, texcoord.y) + vec2<f32>(0.0, -2.0 * rt_metrics.y)).r;
    factor.y -= rounding.y * sample_edges(vec2<f32>(texcoord.z, texcoord.w) + vec2<f32>(rt_metrics.x, -2.0 * rt_metrics.y)).r;
    return weights_in * clamp(factor, vec2<f32>(0.0), vec2<f32>(1.0));
}

fn detect_vertical_corner_pattern(weights_in: vec2<f32>, texcoord: vec4<f32>, d: vec2<f32>) -> vec2<f32> {
    let left_right = step(d, vec2<f32>(d.y, d.x));
    var rounding = (1.0 - SMAA_CORNER_ROUNDING_NORM) * left_right;
    rounding = rounding / max(left_right.x + left_right.y, 1e-5);
    var factor = vec2<f32>(1.0);
    factor.x -= rounding.x * sample_edges(vec2<f32>(texcoord.x, texcoord.y) + vec2<f32>(rt_metrics.x, 0.0)).g;
    factor.x -= rounding.y * sample_edges(vec2<f32>(texcoord.z, texcoord.w) + vec2<f32>(rt_metrics.x, rt_metrics.y)).g;
    factor.y -= rounding.x * sample_edges(vec2<f32>(texcoord.x, texcoord.y) + vec2<f32>(-2.0 * rt_metrics.x, 0.0)).g;
    factor.y -= rounding.y * sample_edges(vec2<f32>(texcoord.z, texcoord.w) + vec2<f32>(-2.0 * rt_metrics.x, rt_metrics.y)).g;
    return weights_in * clamp(factor, vec2<f32>(0.0), vec2<f32>(1.0));
}

// ─── Main (reference: SMAABlendingWeightCalculationPS, SMAA 1x) ────────────

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims_u = textureDimensions(weights_tex);
    let dims = vec2<i32>(i32(dims_u.x), i32(dims_u.y));
    let coords = vec2<i32>(gid.xy);
    if (coords.x >= dims.x || coords.y >= dims.y) {
        return;
    }
    rt_metrics = vec4<f32>(1.0 / f32(dims.x), 1.0 / f32(dims.y), f32(dims.x), f32(dims.y));
    let texcoord = (vec2<f32>(coords) + vec2<f32>(0.5)) * rt_metrics.xy;
    let pixcoord = vec2<f32>(coords) + vec2<f32>(0.5);
    let subsample_indices = vec4<f32>(0.0); // SMAA 1x

    var weights = vec4<f32>(0.0);
    var e = textureLoad(edges_tex, coords, 0).rg;

    if (e.g > 0.0) { // Edge at north
        // Diagonals have both north and west edges; searching one boundary is
        // enough — give diagonals priority.
        let diag = calculate_diag_weights(texcoord, e, subsample_indices);
        weights = vec4<f32>(diag.x, diag.y, weights.z, weights.w);

        if (weights.x + weights.y == 0.0) {
            // ── Orthogonal, horizontal edge ──
            var d = vec2<f32>(0.0);
            // Search end bounds are the search START +- 2*STEPS (the start
            // already carries the crossing offsets — reference offset[2]).
            let left_x = search_x_left(
                texcoord + vec2<f32>(-0.25 * rt_metrics.x, -0.125 * rt_metrics.y),
                texcoord.x + (-0.25 - 2.0 * SMAA_MAX_SEARCH_STEPS) * rt_metrics.x,
            );
            // @CROSSING_OFFSET: the crossing fetch samples a QUARTER texel up,
            // bilinearly mixing the two candidate crossing edgels into the
            // {0, 0.25, 0.75, 1} code AreaTex's round(4e) columns expect.
            let row_y = texcoord.y - 0.25 * rt_metrics.y;
            d.x = left_x;

            let e1 = sample_edges(vec2<f32>(left_x, row_y)).r;

            let right_x = search_x_right(
                texcoord + vec2<f32>(1.25 * rt_metrics.x, -0.125 * rt_metrics.y),
                texcoord.x + (1.25 + 2.0 * SMAA_MAX_SEARCH_STEPS) * rt_metrics.x,
            );
            d.y = right_x;

            d = abs(round(rt_metrics.z * d - pixcoord.x));
            let sqrt_d = sqrt(d);

            let e2 = sample_edges(vec2<f32>(right_x + rt_metrics.x, row_y)).r;

            let ortho = area_ortho(sqrt_d, e1, e2, subsample_indices.y);
            let corner_coords = vec4<f32>(left_x, texcoord.y, right_x, texcoord.y);
            let rounded = detect_horizontal_corner_pattern(ortho, corner_coords, d);
            weights = vec4<f32>(rounded.x, rounded.y, weights.z, weights.w);
        } else {
            e.r = 0.0; // Skip vertical processing when a diagonal was found.
        }
    }

    if (e.r > 0.0) { // Edge at west
        // ── Orthogonal, vertical edge ──
        var d = vec2<f32>(0.0);
        let top_y = search_y_up(
            texcoord + vec2<f32>(-0.125 * rt_metrics.x, -0.25 * rt_metrics.y),
            texcoord.y + (-0.25 - 2.0 * SMAA_MAX_SEARCH_STEPS) * rt_metrics.y,
        );
        let col_x = texcoord.x - 0.25 * rt_metrics.x; // @CROSSING_OFFSET
        d.x = top_y;

        let e1 = sample_edges(vec2<f32>(col_x, top_y)).g;

        let bottom_y = search_y_down(
            texcoord + vec2<f32>(-0.125 * rt_metrics.x, 1.25 * rt_metrics.y),
            texcoord.y + (1.25 + 2.0 * SMAA_MAX_SEARCH_STEPS) * rt_metrics.y,
        );
        d.y = bottom_y;

        d = abs(round(rt_metrics.w * d - pixcoord.y));
        let sqrt_d = sqrt(d);

        let e2 = sample_edges(vec2<f32>(col_x, bottom_y + rt_metrics.y)).g;

        let ortho = area_ortho(sqrt_d, e1, e2, subsample_indices.x);
        let corner_coords = vec4<f32>(texcoord.x, top_y, texcoord.x, bottom_y);
        let rounded = detect_vertical_corner_pattern(ortho, corner_coords, d);
        weights = vec4<f32>(weights.x, weights.y, rounded.x, rounded.y);
    }

    textureStore(weights_tex, coords, weights);
}
