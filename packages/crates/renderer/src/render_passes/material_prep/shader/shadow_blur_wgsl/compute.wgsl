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
// (a plain per-component box is correct — no unpack needed).

const BLUR_LAYERS: u32 = {{ shadow_visibility_layers }}u;
// Half-width of the 1D kernel (per axis). 4 → a 9-tap separable blur ≈ 9×9.
const BLUR_RADIUS: i32 = 4;
// Spatial Gaussian variance (in texels²) for the kernel falloff.
const BLUR_SPATIAL_VAR: f32 = 4.0;
// Edge-stop: a neighbour whose linear view-z differs from the centre by this
// FRACTION contributes ~e^-0.5. Relative (not absolute) so it is scale- and
// distance-invariant — a grazing floor's own gradient stays under it while a
// silhouette/sky jump blows past it.
const BLUR_DEPTH_REL_SIGMA: f32 = 0.05;

// Reconstruct linear view-space Z (metres, positive in front) from the depth
// texture — same unprojection cs_prep uses, reduced to just the Z we need.
// Only the Z and W of the unprojected point matter, so the caller hoists rows
// 2 and 3 of inv_proj (one `camera_from_raw` per pixel) and we do two dot
// products per tap instead of a full mat4x4 * vec4.
fn blur_view_z(inv_proj_row_z: vec4<f32>, inv_proj_row_w: vec4<f32>, coords: vec2<i32>, dims: vec2<f32>) -> f32 {
    let depth = textureLoad(blur_depth_tex, coords, 0);
    let pix_uv = (vec2<f32>(coords) + vec2<f32>(0.5, 0.5)) / dims;
    let p = vec4<f32>(pix_uv.x * 2.0 - 1.0, 1.0 - pix_uv.y * 2.0, depth, 1.0);
    let view_z = dot(inv_proj_row_z, p);
    let view_w = dot(inv_proj_row_w, p);
    return -(view_z / max(view_w, 1e-8));
}

fn blur_axis(gid: vec2<u32>, dir: vec2<i32>) {
    let dims_u = textureDimensions(blur_src).xy;
    if (gid.x >= dims_u.x || gid.y >= dims_u.y) {
        return;
    }
    let dims_i = vec2<i32>(dims_u);
    let dims_f = vec2<f32>(dims_u);
    // Rows 2 and 3 of inv_proj — the only rows view-z reconstruction needs.
    // (inv_proj[col][row]; column-major, so row r = the r-th component of each column.)
    let inv_proj = camera_from_raw(blur_camera_raw).inv_proj;
    let inv_proj_row_z = vec4<f32>(inv_proj[0].z, inv_proj[1].z, inv_proj[2].z, inv_proj[3].z);
    let inv_proj_row_w = vec4<f32>(inv_proj[0].w, inv_proj[1].w, inv_proj[2].w, inv_proj[3].w);
    let c = vec2<i32>(gid);
    let center_z = blur_view_z(inv_proj_row_z, inv_proj_row_w, c, dims_f);
    let z_sigma = max(center_z, 0.05) * BLUR_DEPTH_REL_SIGMA;
    let inv_two_z_var = 1.0 / (2.0 * z_sigma * z_sigma);

    // Edge-stopping is geometry-only (depth), so the per-tap spatial × depth
    // weight is identical for every layer — compute it ONCE, reuse across the
    // `BLUR_LAYERS` packed-light layers.
    var taps: array<vec2<i32>, 8>;      // up to BLUR_RADIUS*2 neighbour coords
    var weights: array<f32, 8>;
    var tap_count: u32 = 0u;
    for (var k: i32 = 1; k <= BLUR_RADIUS; k = k + 1) {
        let gw = exp(-f32(k * k) / (2.0 * BLUR_SPATIAL_VAR));
        for (var s: i32 = -1; s <= 1; s = s + 2) {
            let nc = clamp(c + dir * (k * s), vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
            let nz = blur_view_z(inv_proj_row_z, inv_proj_row_w, nc, dims_f);
            let dz = nz - center_z;
            let wz = exp(-(dz * dz) * inv_two_z_var);
            taps[tap_count] = nc;
            weights[tap_count] = gw * wz;
            tap_count = tap_count + 1u;
        }
    }

    for (var l: u32 = 0u; l < BLUR_LAYERS; l = l + 1u) {
        // Centre tap (weight 1).
        var sum = textureLoad(blur_src, c, i32(l), 0);
        var wsum = 1.0;
        for (var t: u32 = 0u; t < tap_count; t = t + 1u) {
            sum = sum + textureLoad(blur_src, taps[t], i32(l), 0) * weights[t];
            wsum = wsum + weights[t];
        }
        textureStore(blur_dst, c, i32(l), sum / wsum);
    }
}

@compute @workgroup_size(8, 8)
fn cs_blur_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    blur_axis(gid.xy, vec2<i32>(1, 0));
}

@compute @workgroup_size(8, 8)
fn cs_blur_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    blur_axis(gid.xy, vec2<i32>(0, 1));
}
