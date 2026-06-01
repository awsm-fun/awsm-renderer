// --- constants & helpers -------------------------------------
const PI      : f32 = 3.1415926535897932384626433832795;
const TAU     : f32 = 6.283185307179586476925286766559; // 2*PI
const EPSILON : f32 = 1e-4;
const F32_MAX = 2139095039u;
const U32_MAX = 4294967295u;

fn saturate(x: f32) -> f32 { return clamp(x, 0.0, 1.0); }
fn saturate3(v: vec3<f32>) -> vec3<f32> { return clamp(v, vec3<f32>(0.0), vec3<f32>(1.0)); }

// attenuation for a point/spot light per KHR_lights_punctual:
//   attenuation = max(min(1 - (dist/range)^4, 1), 0) / dist^2
// When range == 0, falloff is omitted (light has unlimited range).
fn inverse_square(range: f32, dist: f32) -> f32 {
    let inv_sq = 1.0 / max(dist * dist, 1e-4);
    if (range <= 0.0) {
        return inv_sq;
    }
    let ratio = dist / range;
    let ratio4 = ratio * ratio * ratio * ratio;
    return saturate(1.0 - ratio4) * inv_sq;
}

fn safe_normalize(normal: vec3<f32>) -> vec3<f32> {
    let len_sq = dot(normal, normal);
    if (len_sq > 0.0) {
        return normal * inverseSqrt(len_sq);
    }
    // fallback: up vector to avoid NaNs; scene lighting expects unit normal
    return vec3<f32>(0.0, 0.0, 1.0);
}

fn join32(lo: u32, hi: u32) -> u32 {
  return (hi << 16u) | (lo & 0xFFFFu);
}

fn split16(x: u32) -> vec2<u32> {
  let lo = x & 0xFFFFu;
  let hi = x >> 16u;
  return vec2<u32>(lo, hi);
}

// ------------------------------------------------------------
// Octahedral normal encoding (unit normal <-> vec2)
// Encodes a unit normal into 2 channels with minimal distortion
// ------------------------------------------------------------
fn encode_octahedral(n_in: vec3<f32>) -> vec2<f32> {
    var n = n_in / (abs(n_in.x) + abs(n_in.y) + abs(n_in.z));
    if (n.z < 0.0) {
        let one = vec2<f32>(1.0, 1.0);
        let sgn = sign(n.xy);
        let wrapped = (one - abs(n.yx)) * sgn;
        n = vec3<f32>(wrapped.x, wrapped.y, n.z);
    }
    return n.xy * 0.5 + vec2<f32>(0.5, 0.5);
}

fn decode_octahedral(e: vec2<f32>) -> vec3<f32> {
    let f = e * 2.0 - vec2<f32>(1.0, 1.0);
    var n = vec3<f32>(f.x, f.y, 1.0 - abs(f.x) - abs(f.y));
    let t = clamp(-n.z, 0.0, 1.0);

    // Add -t where n.xy >= 0, else +t (per component)
    let vx = select(t, -t, n.x >= 0.0);
    let vy = select(t, -t, n.y >= 0.0);
    n = vec3<f32>(n.x + vx, n.y + vy, n.z);

    return normalize(n);
}

// ------------------------------------------------------------
// Stable canonical tangent/bitangent basis (Frisvad-style)
// Generates an orthonormal basis from a normal vector
// ------------------------------------------------------------
struct TB { t: vec3<f32>, b: vec3<f32> };

fn canonical_tb(n: vec3<f32>) -> TB {
    if (n.z < -0.9999999) {
        return TB(vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(-1.0,  0.0, 0.0));
    } else {
        let a  = 1.0 / (1.0 + n.z);
        let bb = -n.x * n.y * a;
        let t  = vec3<f32>(1.0 - n.x * n.x * a, bb, -n.x);
        let b  = vec3<f32>(bb, 1.0 - n.y * n.y * a, -n.y);
        return TB(t, b);
    }
}

// ------------------------------------------------------------
// TBN packing/unpacking
// Pack: N (unit), T (unit), s (+1 right-handed, -1 left-handed)
// -> vec4<f32> : [octN.xy, angleU, signU]
// angleU = theta in [0,1] where theta is rotation of T in N's plane
// signU  = 1 for s>0, 0 for s<=0
// ------------------------------------------------------------
fn pack_normal_tangent(N: vec3<f32>, T: vec3<f32>, s: f32) -> vec4<f32> {
    let octN   = encode_octahedral(N);
    let tb     = canonical_tb(N);
    let x      = dot(T, tb.t);
    let y      = dot(T, tb.b);
    let theta  = atan2(y, x);                 // [-PI, PI]
    let angleU = (theta + PI) / TAU;          // [0,1]
    let signU  = select(0.0, 1.0, s > 0.0);   // 0 or 1
    return vec4<f32>(octN.x, octN.y, angleU, signU);
}

// Unpack: vec4<f32> -> N, T, B (orthonormal)
struct TBN { N: vec3<f32>, T: vec3<f32>, B: vec3<f32> };

fn unpack_normal_tangent(rgba: vec4<f32>) -> TBN {
    let N     = decode_octahedral(rgba.xy);
    let theta = rgba.z * TAU - PI;           // [-PI, PI]
    let s     = select(-1.0, 1.0, rgba.w >= 0.5);

    let tb0 = canonical_tb(N);
    let T   = normalize(cos(theta) * tb0.t + sin(theta) * tb0.b);
    let B   = s * normalize(cross(N, T));
    return TBN(N, T, B);
}

// Convert relative indices to absolute indices (0 stays 0)
fn abs_index(base_index: u32, relative_index: u32) -> u32 {
    return select(0u, base_index + relative_index, relative_index != 0u);
}

// -------------------------------------------------------------
// IOR and Refraction Utilities
// -------------------------------------------------------------
// Moved here from brdf.wgsl so they're always available: the transparent pass's
// transmission helpers (sample_transmission_background_for_ior) use them even on
// non-PBR pipelines where brdf.wgsl is gated out. They're generic utilities, not
// BRDF lobes. See docs/plans/SKINNY-MATERIALS.md.

// Get effective IOR value, defaulting to 1.5 when invalid (< 1.0).
// IOR = 1.0 is valid (air, no refraction), IOR < 1.0 is physically invalid.
fn effective_ior(ior: f32) -> f32 {
    return select(ior, 1.5, ior < 1.0);
}

// Convert index of refraction to F0 (reflectance at normal incidence).
// Default IOR of 1.5 yields F0 = 0.04 (standard dielectric).
fn ior_to_f0(ior: f32) -> f32 {
    let ior_val = effective_ior(ior);
    let ratio = (ior_val - 1.0) / (ior_val + 1.0);
    return ratio * ratio;
}

// Calculate refracted direction using Snell's law.
// Returns vec3(0) if total internal reflection occurs.
fn refract_direction(incident: vec3<f32>, normal: vec3<f32>, eta: f32) -> vec3<f32> {
    // Optimization: no refraction when eta ≈ 1.0 (same medium)
    if (abs(eta - 1.0) < 0.001) {
        return incident;
    }
    let cos_i = -dot(incident, normal);
    let sin_t2 = eta * eta * (1.0 - cos_i * cos_i);
    if (sin_t2 > 1.0) {
        return vec3<f32>(0.0);  // Signal TIR to caller
    }
    let cos_t = sqrt(1.0 - sin_t2);
    return eta * incident + (eta * cos_i - cos_t) * normal;
}
