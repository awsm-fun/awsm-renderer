// Separable edge-aware blur on the packed shadow-visibility array — the
// optional shadow denoise pass. `cs_blur_h` then `cs_blur_v` run in sequence
// (H writes the temp, V writes back into prep_shadow_visibility), each a 1D
// Gaussian gated by a RELATIVE linear-depth similarity weight: neighbours on
// the same continuous surface contribute (smoothing the PCSS/soft penumbra
// speckle), while neighbours across a silhouette — or sky — fall off sharply
// (their view-z differs by a large fraction) so shadow never bleeds across a
// geometry edge.
//
// `BLUR_LAYERS` = ceil(K/4) packed visibility layers; each Rgba8unorm channel
// is an independent light's visibility, so every channel blurs independently
// (a plain per-component sum is correct — no unpack of the light slots needed).
//
// ── Shared-memory tiling ──────────────────────────────────────────────────────
// A naive version loads, per pixel per axis, `2R+1` depth texels (the depth
// target is 4× multisampled under MSAA, so those loads are expensive) plus
// `2R+1` visibility texels. Instead each 16×16 workgroup loads its tile plus a
// `BLUR_RADIUS` apron along the blur axis into workgroup memory ONCE — every
// texel's linear view-z is reconstructed once and its visibility packed to a
// u32 (lossless: the source is Rgba8unorm) — and the taps read from there.
// H and V use the same element count (`TILE × (TILE + 2R)`), just transposed,
// so they share the arrays.
//
// ── Sky ──────────────────────────────────────────────────────────────────────
// `cs_prep` never writes shadow visibility for sky pixels and nothing reads it
// there, so sky centres are skipped outright (no taps, no store), and a sky
// neighbour contributes weight 0. Sky is detected by the raw depth equalling
// the depth convention's clear value (`SKY_DEPTH`).

const BLUR_LAYERS: u32 = {{ shadow_visibility_layers }}u;
// Half-width of the 1D kernel (per axis): a (2R+1)-tap separable blur.
const BLUR_RADIUS: i32 = {{ blur_radius }};
// Spatial Gaussian weights for tap distance 0..=R, variance R²/4 (so R=4 gives
// the classic variance-4 kernel, and the outer tap's weight is e^-2 at any R).
const BLUR_GAUSS: array<f32, {{ blur_radius_plus_one }}> = array<f32, {{ blur_radius_plus_one }}>({{ gaussian_weights }});
// Edge-stop: a neighbour whose linear view-z differs from the centre by this
// FRACTION contributes ~e^-0.5. Relative (not absolute) so it is scale- and
// distance-invariant — a grazing floor's own gradient stays under it while a
// silhouette/sky jump blows past it.
const BLUR_DEPTH_REL_SIGMA: f32 = 0.05;
// Raw depth of an empty (sky) pixel: the depth convention's clear value.
const SKY_DEPTH: f32 = {{ sky_depth }};
// Stored in the view-z tile for sky texels (real view-z is always positive).
const SKY_Z: f32 = -1.0;

const TILE: i32 = 16;
// Tile texel count: TILE × (TILE + 2R) — identical for H and V.
const TILE_N: u32 = {{ tile_texels }}u;

var<workgroup> tile_z: array<f32, {{ tile_texels }}>;
var<workgroup> tile_vis: array<u32, {{ tile_vis_entries }}>;

// Linear view-space Z (metres, positive in front) of one depth texel, or SKY_Z.
// Only rows 2 and 3 of inv_proj matter for Z/W, so the caller hoists them and
// this is two dot products.
fn blur_view_z(inv_proj_row_z: vec4<f32>, inv_proj_row_w: vec4<f32>, coords: vec2<i32>, dims: vec2<f32>) -> f32 {
    let depth = textureLoad(blur_depth_tex, coords, 0);
    if (depth == SKY_DEPTH) {
        return SKY_Z;
    }
    let pix_uv = (vec2<f32>(coords) + vec2<f32>(0.5, 0.5)) / dims;
    let p = vec4<f32>(pix_uv.x * 2.0 - 1.0, 1.0 - pix_uv.y * 2.0, depth, 1.0);
    let view_z = dot(inv_proj_row_z, p);
    let view_w = dot(inv_proj_row_w, p);
    return -(view_z / max(view_w, 1e-8));
}

// `horizontal` is a literal at both call sites, so every branch on it is
// uniform. The barrier is reached by EVERY invocation (out-of-image ones
// included — they still load their share of the tile, with clamped coords);
// the bounds / sky early-outs come only after it.
fn blur_axis(gid: vec2<u32>, lid: vec2<u32>, lidx: u32, wid: vec2<u32>, horizontal: bool) {
    let dims_u = textureDimensions(blur_src).xy;
    let dims_i = vec2<i32>(dims_u);
    let dims_f = vec2<f32>(dims_u);
    // Rows 2 and 3 of inv_proj (column-major: row r = component r of each column).
    let inv_proj = blur_camera_raw.inv_proj;
    let inv_proj_row_z = vec4<f32>(inv_proj[0].z, inv_proj[1].z, inv_proj[2].z, inv_proj[3].z);
    let inv_proj_row_w = vec4<f32>(inv_proj[0].w, inv_proj[1].w, inv_proj[2].w, inv_proj[3].w);

    // Tile geometry: the apron runs along the blur axis.
    let base = vec2<i32>(wid) * TILE;
    var origin: vec2<i32>;
    var tile_w: i32;
    var stride: i32;
    if (horizontal) {
        origin = base - vec2<i32>(BLUR_RADIUS, 0);
        tile_w = TILE + 2 * BLUR_RADIUS;
        stride = 1;
    } else {
        origin = base - vec2<i32>(0, BLUR_RADIUS);
        tile_w = TILE;
        stride = TILE;
    }

    // ── Cooperative load: each texel's view-z + packed visibility, once. ──
    // Border texels clamp to the image edge, matching a clamped tap.
    for (var i: u32 = lidx; i < TILE_N; i = i + 256u) {
        let t = vec2<i32>(i32(i) % tile_w, i32(i) / tile_w);
        let c = clamp(origin + t, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
        tile_z[i] = blur_view_z(inv_proj_row_z, inv_proj_row_w, c, dims_f);
        for (var l: u32 = 0u; l < BLUR_LAYERS; l = l + 1u) {
            tile_vis[i * BLUR_LAYERS + l] = pack4x8unorm(textureLoad(blur_src, c, i32(l), 0));
        }
    }
    workgroupBarrier();

    if (gid.x >= dims_u.x || gid.y >= dims_u.y) {
        return;
    }
    var local_pos: vec2<i32>;
    if (horizontal) {
        local_pos = vec2<i32>(lid) + vec2<i32>(BLUR_RADIUS, 0);
    } else {
        local_pos = vec2<i32>(lid) + vec2<i32>(0, BLUR_RADIUS);
    }
    let ci = local_pos.y * tile_w + local_pos.x;
    let center_z = tile_z[ci];
    if (center_z == SKY_Z) {
        return;
    }
    let z_sigma = max(center_z, 0.05) * BLUR_DEPTH_REL_SIGMA;
    let inv_two_z_var = 1.0 / (2.0 * z_sigma * z_sigma);

    // Edge-stopping is geometry-only (depth), so the per-tap spatial × depth
    // weight is identical for every layer — compute it ONCE per tap.
    var taps: array<i32, {{ tap_count }}>;
    var weights: array<f32, {{ tap_count }}>;
    var tap_count: u32 = 0u;
    for (var k: i32 = 1; k <= BLUR_RADIUS; k = k + 1) {
        let gw = BLUR_GAUSS[k];
        for (var s: i32 = -1; s <= 1; s = s + 2) {
            let ni = ci + s * k * stride;
            let nz = tile_z[ni];
            let dz = nz - center_z;
            let wz = select(exp(-(dz * dz) * inv_two_z_var), 0.0, nz == SKY_Z);
            taps[tap_count] = ni;
            weights[tap_count] = gw * wz;
            tap_count = tap_count + 1u;
        }
    }

    let c = vec2<i32>(gid);
    for (var l: u32 = 0u; l < BLUR_LAYERS; l = l + 1u) {
        // Centre tap (weight BLUR_GAUSS[0] = 1).
        var sum = unpack4x8unorm(tile_vis[u32(ci) * BLUR_LAYERS + l]);
        var wsum = 1.0;
        for (var t: u32 = 0u; t < tap_count; t = t + 1u) {
            sum = sum + unpack4x8unorm(tile_vis[u32(taps[t]) * BLUR_LAYERS + l]) * weights[t];
            wsum = wsum + weights[t];
        }
        textureStore(blur_dst, c, i32(l), sum / wsum);
    }
}

@compute @workgroup_size(16, 16)
fn cs_blur_h(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(local_invocation_index) lidx: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    blur_axis(gid.xy, lid.xy, lidx, wid.xy, true);
}

@compute @workgroup_size(16, 16)
fn cs_blur_v(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(local_invocation_index) lidx: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    blur_axis(gid.xy, lid.xy, lidx, wid.xy, false);
}
