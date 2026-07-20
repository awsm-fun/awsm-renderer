// ============================================================================
// mipmap.wgsl — Gradient-based texture sampling for compute shaders
// ============================================================================
//
// Deferred shading happens in a compute pass, which has no fixed-function
// dpdx/dpdy. So the geometry (visibility-buffer) pass computes the screen-space
// barycentric derivatives there — where hardware derivatives DO exist — and
// stores them in the `barycentric_derivatives` G-buffer. This helper turns those
// stored d(bary)/d(screen) into UV gradients for `textureSampleGrad`:
// 1. Read the per-pixel d(bary)/d(screen) from the G-buffer (`bary_derivs` arg).
//    (Geometry takes them from a CENTER-sampled barycentric — `barycentric_center`
//    in geometry_wgsl — since derivatives of a centroid varying are undefined.)
// 2. Apply chain rule: d(UV)/d(screen) = d(UV)/d(bary) × d(bary)/d(screen),
//    using the triangle's three vertex UVs.
// 3. Hand the gradients to textureSampleGrad → hardware mip / anisotropic
//    selection (no manual LOD, no triangle seams).
//
// NOTE: an earlier revision computed the barycentric derivatives analytically
// from screen-space-projected triangle vertices; that was replaced by the stored
// hardware derivatives above. (UV address-mode unwrapping for repeat/mirror was
// also tried here and removed — see `unwrap_repeat`/`mirror_*` below, kept unused
// for reference.)
// ============================================================================

// ─────────────────────────────────────────────────────────────────────────────
// Orthographic Anisotropic Filtering Configuration - REMOVED
// ─────────────────────────────────────────────────────────────────────────────
// These constants were removed as the orthographic correction was causing more
// problems than it solved. Hardware anisotropic filtering should handle tilted
// surfaces automatically.
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// Shared structs
// ─────────────────────────────────────────────────────────────────────────────
struct UvDerivs {
    ddx: vec2<f32>,  // (dudx, dvdx)
    ddy: vec2<f32>,  // (dudy, dvdy)
}

struct UV3 { u0: vec2<f32>, u1: vec2<f32>, u2: vec2<f32> }

struct MirrorLocal { uv: vec2<f32>, slope: vec2<f32> } // slope per axis is ±1

// Convert a UV coordinate from mirror-repeat space to texture space [0,1)
// This handles the reflection: [0,1) maps to [0,1), [1,2) maps to [1,0), [2,3) maps to [0,1), etc.
fn mirror_to_texture_space(uv: f32) -> f32 {
    let k = floor(uv);
    let frac = uv - k;
    let is_odd = (i32(k) & 1) != 0;
    // In odd segments, reflect: texture_coord = 1.0 - frac
    // In even segments, pass through: texture_coord = frac
    return select(frac, 1.0 - frac, is_odd);
}

// Unwrap aX relative to a0 for repeat textures
// This makes vertices continuous when they cross the 0→1 boundary,
// but preserves large spans (e.g., texture repeating multiple times)
fn unwrap_repeat_axis(a0: f32, aX: f32) -> f32 {
    let d = aX - a0;

    // Compute what the distance would be with ±1 shifts
    // Choose whichever gives the smallest absolute distance
    let d_minus = d - 1.0;
    let d_plus = d + 1.0;

    // But only apply a shift if it makes a "significant" improvement (> 0.5 reduction)
    // This prevents collapsing large spans while still unwrapping boundaries
    if (abs(d_plus) < abs(d) - 0.5) {
        return aX + 1.0;
    } else if (abs(d_minus) < abs(d) - 0.5) {
        return aX - 1.0;
    } else {
        return aX;
    }
}


fn unwrap_repeat(u0: vec2<f32>, u1: vec2<f32>, u2: vec2<f32>) -> UV3 {
    return UV3(
        u0,
        vec2<f32>(unwrap_repeat_axis(u0.x, u1.x), unwrap_repeat_axis(u0.y, u1.y)),
        vec2<f32>(unwrap_repeat_axis(u0.x, u2.x), unwrap_repeat_axis(u0.y, u2.y))
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Alternative mirror mode approach (not currently used, kept for reference)
// ─────────────────────────────────────────────────────────────────────────────
// These functions create a continuous "sawtooth" space for mirror coordinates,
// but require slope adjustment which complicates the implementation.
// Current approach uses mirror_to_texture_space() instead, which is simpler.

fn mirror_linearize_axis(a: f32) -> vec2<f32> {
    // Returns (u_lin, slope)
    let k = floor(a);
    let frac = a - k; // [0,1)
    let is_odd = (i32(k) & 1) != 0;
    // Map to a monotonic coordinate (continuous in "mirror space"):
    let u_lin = select(frac, 1.0 - frac, is_odd) + k;
    let s = select(1.0, -1.0, is_odd);
    return vec2<f32>(u_lin, s);
}

fn mirror_linearize(uv: vec2<f32>) -> MirrorLocal {
    let x = mirror_linearize_axis(uv.x);
    let y = mirror_linearize_axis(uv.y);
    return MirrorLocal(
        vec2<f32>(x.x, y.x),
        vec2<f32>(x.y, y.y)
    );
}

fn get_uv_derivatives(
    barycentric: vec3<f32>,
    bary_derivs: vec4<f32>,         // (db1dx, db1dy, db2dx, db2dy)
    tri: vec3<u32>,
    attribute_data_offset: u32,
    vertex_stride: u32,
    uv_sets_index: u32,
    tex_info: TextureInfo,
    world_normal: vec3<f32>,        // Surface normal in world space
    view_matrix: mat4x4<f32>        // Camera view matrix
) -> UvDerivs {
    let uv_set_index = tex_info.uv_set_index;

    let uv0 = _texture_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, tri.x, vertex_stride, uv_sets_index);
    let uv1 = _texture_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, tri.y, vertex_stride, uv_sets_index);
    let uv2 = _texture_uv_per_vertex(attribute_data_offset, tex_info.uv_set_index, tri.z, vertex_stride, uv_sets_index);

    // Barycentric derivatives from geometry pass
    // bary_derivs contains: (d(bary.x)/dx, d(bary.x)/dy, d(bary.y)/dx, d(bary.y)/dy)
    // Where bary is the vec2 barycentric coordinate being interpolated
    // The full vec3 barycentric is: (bary.x, bary.y, 1 - bary.x - bary.y)
    let dAlphaDx = bary_derivs.x;  // d(bary.x)/dx
    let dAlphaDy = bary_derivs.y;  // d(bary.x)/dy
    let dBetaDx = bary_derivs.z;   // d(bary.y)/dx
    let dBetaDy = bary_derivs.w;   // d(bary.y)/dy

    // If nearly zero derivatives, short-circuit (selects base mip).
    let m = abs(dAlphaDx) + abs(dAlphaDy) + abs(dBetaDx) + abs(dBetaDy);
    if (m < 1e-20) {
        return UvDerivs(vec2<f32>(0.0), vec2<f32>(0.0));
    }

    // Third barycentric component derivative: d(1 - alpha - beta)/d(x or y)
    let dGammaDx = -dAlphaDx - dBetaDx;
    let dGammaDy = -dAlphaDy - dBetaDy;

    // Make the THREE vertex UVs locally continuous per axis based on the address mode.
    var U0 = uv0;
    var U1 = uv1;
    var U2 = uv2;

    // NOTE: UV unwrapping for repeat/mirror modes removed.
    // The address modes apply to the FINAL transformed coordinates.
    // Unwrapping in attribute space causes incorrect gradients when transforms are present.

    // Chain rule: d(UV)/d(screen) = d(UV)/d(bary) × d(bary)/d(screen)
    // UV = alpha*uv0 + beta*uv1 + gamma*uv2
    // So: d(UV)/dx = uv0*dAlpha/dx + uv1*dBeta/dx + uv2*dGamma/dx
    var dudx = U0.x * dAlphaDx + U1.x * dBetaDx + U2.x * dGammaDx;
    var dvdx = U0.y * dAlphaDx + U1.y * dBetaDx + U2.y * dGammaDx;

    var dudy = U0.x * dAlphaDy + U1.x * dBetaDy + U2.x * dGammaDy;
    var dvdy = U0.y * dAlphaDy + U1.y * dBetaDy + U2.y * dGammaDy;

    // ========================================================================
    // Orthographic projection correction - REMOVED
    // ========================================================================
    // This was attempting to fix anisotropic filtering for orthographic cameras,
    // but it causes problems:
    // - Makes assumptions about camera projection
    // - Causes blur or over-sharpening depending on settings
    // - Looks bad with perspective projection
    //
    // Hardware anisotropic filtering (via max_anisotropy sampler setting) should
    // handle this automatically when proper gradients are provided to textureSampleGrad.
    // If thin lines still swim, that may be an inherent limitation of the compute
    // shader gradient calculation approach.

    // LOD bias simulation: Scale gradients to shift mip selection
    // LOD = log2(rho) where rho is gradient magnitude
    // LOD_biased = LOD + bias
    //
    // To achieve bias effect, scale gradients:
    //   bias = -0.5 → scale = 1.414 (sharper: divide gradients by 1.414)
    //   bias = -1.0 → scale = 2.0 (much sharper: divide gradients by 2.0)
    //   bias = +0.5 → scale = 0.707 (blurrier: multiply gradients by 1.414)
    //   bias =  0.0 → scale = 1.0 (no change)
    //
    // Current setting: -0.5 bias (sharper, less swimming on thin lines)
    // let lod_bias_scale = 1.414;  // 2^0.5
    // dudx /= lod_bias_scale;
    // dvdx /= lod_bias_scale;
    // dudy /= lod_bias_scale;
    // dvdy /= lod_bias_scale;

    // NaN/Inf guard. Fail toward BLUR, not sparkle: zero gradients mean LOD 0
    // (the sharpest mip), which turns a degenerate sub-pixel triangle into
    // full-res point-sampling — dash/shimmer aliasing no AA can fix. A huge
    // gradient instead selects the coarsest mip: the correct filtering
    // direction when the true footprint is unknown-but-large. (The zero-`m`
    // short-circuit above stays: truly-zero stored derivs mean an UNWRITTEN
    // G-buffer sample, where LOD 0 is the least-wrong choice for magnified
    // texels.)
    let ok = (dudx == dudx) && (dudy == dudy) && (dvdx == dvdx) && (dvdy == dvdy);
    if (!ok) {
        return UvDerivs(vec2<f32>(1.0e3, 0.0), vec2<f32>(0.0, 1.0e3));
    }

    return UvDerivs(vec2<f32>(dudx, dvdx), vec2<f32>(dudy, dvdy));
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API - Gradient Computation
// ─────────────────────────────────────────────────────────────────────────────

{% include "material_opaque_wgsl/helpers/mipmap_pbr.wgsl" %}

// Debug helper: Calculate what mip level would be selected for a given texture
// This mimics the hardware mip selection algorithm
fn debug_calculate_mip_level(
    ddx: vec2<f32>,
    ddy: vec2<f32>,
    texture_size: vec2<u32>
) -> f32 {
    // Convert gradients from UV space [0,1] to texel space
    let ddx_texels = ddx * vec2<f32>(f32(texture_size.x), f32(texture_size.y));
    let ddy_texels = ddy * vec2<f32>(f32(texture_size.x), f32(texture_size.y));

    // Compute gradient magnitudes (texels per pixel)
    let rho_x = length(ddx_texels);
    let rho_y = length(ddy_texels);
    let rho = max(rho_x, rho_y);

    // Hardware mip selection: LOD = log2(rho)
    return log2(max(rho, 1e-6));
}
