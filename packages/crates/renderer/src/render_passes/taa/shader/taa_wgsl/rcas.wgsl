// RCAS: robust contrast-adaptive sharpening (AMD FidelityFX FSR 1, MIT) on
// the TAA output (03 §9). One invocation per output pixel, post size.
//
// Optional, off by default and always off for the ghosting tests (G12): it
// sharpens what the resolve produced, so it must never be what hides a
// resolve that blurs at native resolution.
//
// RCAS works on values in [0, 1]; `taa_out` is linear HDR. It runs in the
// resolve's working space, t = c / (1 + max3(c)) (03 §4.4), and maps back:
// the same compression keeps one HDR highlight from dominating the limiter,
// and the same expansion as the resolve's output.
//
// Plain static WGSL (no template). Apple GPU rules: five fixed taps, no loop,
// no array, no switch.

struct RcasParams {
    // 0 = no sharpening, 1 = FSR's maximum (sharpness 0 stops). FSR's
    // "sharpness in stops" s maps to strength = exp2(-s).
    strength: f32,
    // 1 when this frame's TAA reactive mask is valid (the resolve's
    // `reactive_valid`): only then are HUD pass-through pixels recognised.
    reactive_valid: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> params: RcasParams;
// Linear HDR at post size (`taa_out`).
@group(0) @binding(1) var src_tex: texture_2d<f32>;
// Sharpened linear HDR at post size.
@group(0) @binding(2) var dst_tex: texture_storage_2d<rgba16float, write>;
// The TAA reactive mask (render size). Exactly 1 marks an opaque-path HUD
// pixel, which the resolve passed through unaccumulated: it must not be
// sharpened either (it would come out differently from TAA off).
@group(0) @binding(3) var reactive_tex: texture_2d<f32>;

// The resolve's HUD threshold (`HUD_REACTIVE` in resolve.wgsl).
const HUD_REACTIVE: f32 = 0.999;

// FSR_RCAS_LIMIT: the strongest negative lobe (0.25 - 1/16). Keeps the
// kernel's centre weight positive and the result inside the neighbourhood.
const RCAS_LIMIT: f32 = 0.1875;
// Tonemapped values stay below 1 so the expansion cannot divide by zero
// (the resolve's TONEMAPPED_MAX).
const TONEMAPPED_MAX: f32 = 0.9995;

fn max3(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b));
}

fn tonemap(c: vec3<f32>) -> vec3<f32> {
    return c / (1.0 + max3(c));
}

fn untonemap(t: vec3<f32>) -> vec3<f32> {
    return t / (1.0 - min(max3(t), TONEMAPPED_MAX));
}

// One tap in the working space, clamped to the image; non-finite or negative
// input reads as black (NaN fails the range test).
fn tap(p: vec2<i32>, p_max: vec2<i32>) -> vec3<f32> {
    let c = textureLoad(src_tex, clamp(p, vec2<i32>(0), p_max), 0).rgb;
    let ok = (c >= vec3<f32>(0.0)) & (c <= vec3<f32>(65504.0));
    return min(tonemap(select(vec3<f32>(0.0), c, ok)), vec3<f32>(TONEMAPPED_MAX));
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(dst_tex));
    let p = vec2<i32>(gid.xy);
    if (p.x >= dims.x || p.y >= dims.y) {
        return;
    }
    let p_max = dims - 1;

    // HUD pass-through pixel: copy, unsharpened. The render pixel nearest
    // this output pixel's centre, as the resolve's pass-through picks it.
    if (params.reactive_valid > 0.5) {
        let r_dims = vec2<i32>(textureDimensions(reactive_tex));
        let r_p = clamp(
            vec2<i32>(floor((vec2<f32>(p) + 0.5) * vec2<f32>(r_dims) / vec2<f32>(dims))),
            vec2<i32>(0),
            r_dims - 1,
        );
        if (textureLoad(reactive_tex, r_p, 0).r >= HUD_REACTIVE) {
            textureStore(dst_tex, p, textureLoad(src_tex, p, 0));
            return;
        }
    }

    //    b
    //  d e f
    //    h
    let b = tap(p + vec2<i32>(0, -1), p_max);
    let d = tap(p + vec2<i32>(-1, 0), p_max);
    let e = tap(p, p_max);
    let f = tap(p + vec2<i32>(1, 0), p_max);
    let h = tap(p + vec2<i32>(0, 1), p_max);

    // The lobe that keeps the result inside [min, max] of the ring, per
    // channel (FSR's hitMin / hitMax), then the most limiting channel.
    let mn4 = min(min(b, d), min(f, h));
    let mx4 = max(max(b, d), max(f, h));
    // FSR divides by 4·mx4 and 4·mn4 − 4; guard the two zeros (an all-black
    // ring, an all-white one) so the lobe stays finite.
    let hit_min = min(mn4, e) / max(4.0 * mx4, vec3<f32>(1.0e-6));
    let hit_max = (1.0 - max(mx4, e)) / min(4.0 * mn4 - 4.0, vec3<f32>(-1.0e-6));
    let lobe_rgb = max(-hit_min, hit_max);
    let lobe = max(-RCAS_LIMIT, min(max3(lobe_rgb), 0.0)) * clamp(params.strength, 0.0, 1.0);

    // Weights: lobe on the ring, 1 at the centre, normalised. lobe >= -0.1875,
    // so 4·lobe + 1 >= 0.25.
    let t = (lobe * (b + d + f + h) + e) / (4.0 * lobe + 1.0);
    let out = untonemap(clamp(t, vec3<f32>(0.0), vec3<f32>(TONEMAPPED_MAX)));
    textureStore(dst_tex, p, vec4<f32>(out, 1.0));
}
