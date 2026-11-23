// TAA resolve (+ temporal upscaling). One invocation per OUTPUT pixel.
//
// Reads the render-size lit HDR image (after SSR), depth, motion vectors and
// the reactive mask, plus last frame's history and depth, and writes this
// frame's history and `taa_out` (linear HDR at post size: what bloom,
// effects and display read).
//
// Conventions (see `crate::taa`):
//   R = render size, O = post size, both in pixels; x right, y down.
//   j = this frame's jitter in render pixels. Render pixel p holds the scene
//       sampled at UNJITTERED position p + 0.5 - j.
//   v = motion vector in UV units: uv_now - uv_prev of one surface point,
//       both unjittered.
//   n = history length, in the history's alpha.
//
// Every rule below exists because breaking it ghosts or blurs. The list is
// G1-G12 in docs/TAA.md; the tags in the comments refer to it.
//
// Apple GPU rules: the 3x3 neighbourhood is unrolled at template time (no
// loop, no per-invocation array), and no `switch` contains a `return`.

{% include "shared_wgsl/camera.wgsl" %}
{% include "shared_wgsl/taa/sky_velocity.wgsl" %}

struct TaaParams {
    render_size: vec2<f32>,
    post_size: vec2<f32>,
    // Variance-clip half-width in sigmas: still / moving, and the speed
    // (output px / frame) at which `gamma_moving` is reached.
    gamma_still: f32,
    gamma_moving: f32,
    gamma_moving_speed: f32,
    // Relative depth slack of the disocclusion test.
    disocclusion_threshold: f32,
    // History-length cap (N_max).
    max_history: f32,
    // Minimum current weight at `motion_alpha_speed` output px / frame.
    motion_alpha: f32,
    motion_alpha_speed: f32,
    // 1 when render size < post size.
    upscaling: f32,
    // 1 on the first frame after a history reset: no history is read.
    reset: f32,
    // TaaDebugView::shader_id().
    debug_view: f32,
    // Exponent of the reconstruction kernel (crate::taa::RECONSTRUCTION_SHARPNESS).
    kernel_sharpness: f32,
    // 1 when the reactive mask was written this frame; 0 = treat it as zero
    // (nothing reactive was drawn, so the mask was not even cleared).
    reactive_valid: f32,
};

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var<uniform> params: TaaParams;
// Lit HDR after SSR, render size.
@group(0) @binding(2) var color_tex: texture_2d<f32>;
// Depth, render size, single-sample (MSAA is off under TAA).
@group(0) @binding(3) var depth_tex: texture_depth_2d;
// Motion vectors, render size, UV units.
@group(0) @binding(4) var velocity_tex: texture_2d<f32>;
// Last frame's raw depth, at LAST frame's render size (a depth-format copy).
@group(0) @binding(5) var prev_depth_tex: texture_depth_2d;
// Last frame's history, post size: rgb = tonemapped colour, a = n.
@group(0) @binding(6) var history_prev_tex: texture_2d<f32>;
@group(0) @binding(7) var history_sampler: sampler;
// This frame's history.
@group(0) @binding(8) var history_curr_tex: texture_storage_2d<rgba16float, write>;
// Linear HDR at post size.
@group(0) @binding(9) var out_tex: texture_storage_2d<rgba16float, write>;
{% if reactive %}
// Reactive mask, render size: 0 = trust history, 1 = ignore it (G8).
@group(0) @binding(10) var reactive_tex: texture_2d<f32>;
{% endif %}

// ---- Working space (G11) --------------------------------------------------
// Blend, clip and filter on tonemapped colour, so one saturated highlight
// weighs about as much as a white pixel instead of fifty of them.

// The largest tonemapped value the rgba16float history can hold below 1.0 is
// 1 - 2^-11; `untonemap` divides by (1 - max3), so nothing may reach 1.
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

fn rgb_to_ycocg(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        0.25 * c.r + 0.5 * c.g + 0.25 * c.b,
        0.5 * c.r - 0.5 * c.b,
        -0.25 * c.r + 0.5 * c.g - 0.25 * c.b,
    );
}

fn ycocg_to_rgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(c.x + c.y - c.z, c.x + c.z, c.x - c.y - c.z);
}

// A colour that is safe to accumulate: non-finite and negative inputs become
// black. (Comparisons with NaN are false, so NaN fails the range test.)
fn sanitize(c: vec3<f32>) -> vec3<f32> {
    let ok = (c >= vec3<f32>(0.0)) & (c <= vec3<f32>(65504.0));
    return select(vec3<f32>(0.0), c, ok);
}

fn is_sky(depth: f32) -> bool {
    {% if reverse_z %}
    return depth <= 0.0;
    {% else %}
    return depth >= 1.0;
    {% endif %}
}

// Whether raw depth `a` is nearer the camera than `b`.
fn nearer(a: f32, b: f32) -> bool {
    {% if reverse_z %}
    return a > b;
    {% else %}
    return a < b;
    {% endif %}
}

// Positive view distance of a raw depth. Sky is "infinitely far".
fn view_distance(depth: f32, inv_proj: mat4x4<f32>) -> f32 {
    if (is_sky(depth)) {
        return 1.0e30;
    }
    let v = inv_proj * vec4<f32>(0.0, 0.0, depth, 1.0);
    return -v.z / v.w;
}

// ---- History fetch (G4) ---------------------------------------------------
// Catmull-Rom as 5 bilinear fetches: the 9-tap form with the 4 corners
// dropped. Plain bilinear history blurs a little every frame, and blur that
// keeps accumulating looks like ghosting.
fn sample_history(uv: vec2<f32>, size: vec2<f32>) -> vec3<f32> {
    let pos = uv * size;
    let tc1 = floor(pos - 0.5) + 0.5;
    let f = pos - tc1;
    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let tc0 = (tc1 - 1.0) / size;
    let tc3 = (tc1 + 2.0) / size;
    let tc12 = (tc1 + w2 / w12) / size;

    let wa = w12.x * w0.y;
    let wb = w0.x * w12.y;
    let wc = w12.x * w12.y;
    let wd = w3.x * w12.y;
    let we = w12.x * w3.y;
    var sum = textureSampleLevel(history_prev_tex, history_sampler, vec2<f32>(tc12.x, tc0.y), 0.0).rgb * wa;
    sum += textureSampleLevel(history_prev_tex, history_sampler, vec2<f32>(tc0.x, tc12.y), 0.0).rgb * wb;
    sum += textureSampleLevel(history_prev_tex, history_sampler, vec2<f32>(tc12.x, tc12.y), 0.0).rgb * wc;
    sum += textureSampleLevel(history_prev_tex, history_sampler, vec2<f32>(tc3.x, tc12.y), 0.0).rgb * wd;
    sum += textureSampleLevel(history_prev_tex, history_sampler, vec2<f32>(tc12.x, tc3.y), 0.0).rgb * we;
    // The negative lobes can undershoot below 0 and overshoot past the
    // tonemapped range.
    return clamp(sum / (wa + wb + wc + wd + we), vec3<f32>(0.0), vec3<f32>(TONEMAPPED_MAX));
}

fn heat(x: f32) -> vec3<f32> {
    let t = clamp(x, 0.0, 1.0);
    return clamp(
        vec3<f32>(1.5 - abs(4.0 * t - 3.0), 1.5 - abs(4.0 * t - 2.0), 1.5 - abs(4.0 * t - 1.0)),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let post_dims = vec2<i32>(textureDimensions(out_tex));
    let q = vec2<i32>(gid.xy);
    if (q.x >= post_dims.x || q.y >= post_dims.y) {
        return;
    }
    let cam = camera_from_raw(camera_raw);
    let render_dims = vec2<i32>(textureDimensions(color_tex));
    let R = vec2<f32>(render_dims);
    let O = vec2<f32>(post_dims);
    let jitter = cam.jitter;

    // ---- 4.1 Where this output pixel falls in the render image ----------
    let uv_q = (vec2<f32>(q) + 0.5) / O;
    let r_pos = uv_q * R;
    let p0 = clamp(vec2<i32>(floor(r_pos)), vec2<i32>(0), render_dims - 1);
    let p_max = render_dims - 1;

    // ---- 4.2 / 4.3 / 4.6 One pass over the 3x3 render pixels ------------
    // Accumulates the reconstructed current colour, the unweighted YCoCg
    // moments for the clip box, the nearest depth (whose motion vector the
    // pixel uses: dilation, G3) and the largest reactive value.
    var current = vec3<f32>(0.0);
    var weight_sum = 0.0;
    var confidence = 0.0;
    var m1 = vec3<f32>(0.0);
    var m2 = vec3<f32>(0.0);
    var nearest_depth = textureLoad(depth_tex, p0, 0);
    var nearest_p = p0;
    var reactive = 0.0;
    {% for dy in 0..3 %}
    {% for dx in 0..3 %}
    {
        let p = clamp(p0 + vec2<i32>({{ dx }} - 1, {{ dy }} - 1), vec2<i32>(0), p_max);
        let t = tonemap(sanitize(textureLoad(color_tex, p, 0).rgb));
        // This sample really sits at p + 0.5 - j (G2: the jitter is removed
        // here, where the current frame is sampled, never in the vector).
        let d = vec2<f32>(p) + 0.5 - jitter - r_pos;
        let w = exp(-params.kernel_sharpness * dot(d, d));
        current += t * w;
        weight_sum += w;
        confidence = max(confidence, w);
        let y = rgb_to_ycocg(t);
        m1 += y;
        m2 += y * y;
        let z = textureLoad(depth_tex, p, 0);
        if (nearer(z, nearest_depth)) {
            nearest_depth = z;
            nearest_p = p;
        }
        {% if reactive %}
        reactive = max(reactive, textureLoad(reactive_tex, p, 0).r);
        {% endif %}
    }
    {% endfor %}
    {% endfor %}
    current = current / weight_sum;

    // ---- 4.3 Motion vector of the nearest pixel (G1, G3) ----------------
    var v = textureLoad(velocity_tex, nearest_p, 0).xy;
    let sky = is_sky(nearest_depth);
    if (sky) {
        // No geometry anywhere in the 3x3: the vector is the camera's
        // rotation only.
        v = taa_sky_velocity(uv_q, cam);
    }
    let speed_px = length(v * O);

    // ---- 4.5 History fetch (G4) ------------------------------------------
    let uv_hist = uv_q - v;
    let on_screen = all(uv_hist >= vec2<f32>(0.0)) && all(uv_hist <= vec2<f32>(1.0));
    let hist_texel = clamp(vec2<i32>(floor(uv_hist * O)), vec2<i32>(0), post_dims - 1);
    let n_prev = textureLoad(history_prev_tex, hist_texel, 0).a;
    let history_rgb = sample_history(uv_hist, O);

    // ---- 4.7 Disocclusion (G6) -------------------------------------------
    // Last frame's depth where this surface was. Last frame's render pixel p
    // held unjittered position p + 0.5 - prev_jitter, so the texel-space
    // position of `uv_hist` is uv_hist * R + prev_jitter - 0.5; take the
    // nearest of the 2x2 around it (in that texture's own size: the render
    // size may have changed since). Both sides of the test use the nearest
    // depth of their footprint, so a static silhouette does not trip it.
    let prev_dims = vec2<i32>(textureDimensions(prev_depth_tex));
    let pd_max = prev_dims - 1;
    let prev_pos = uv_hist * vec2<f32>(prev_dims) + cam.prev_jitter - 0.5;
    let pd0 = vec2<i32>(floor(prev_pos));
    let pd00 = textureLoad(prev_depth_tex, clamp(pd0, vec2<i32>(0), pd_max), 0);
    let pd10 = textureLoad(prev_depth_tex, clamp(pd0 + vec2<i32>(1, 0), vec2<i32>(0), pd_max), 0);
    let pd01 = textureLoad(prev_depth_tex, clamp(pd0 + vec2<i32>(0, 1), vec2<i32>(0), pd_max), 0);
    let pd11 = textureLoad(prev_depth_tex, clamp(pd0 + vec2<i32>(1, 1), vec2<i32>(0), pd_max), 0);
    {% if reverse_z %}
    let prev_depth = max(max(pd00, pd10), max(pd01, pd11));
    {% else %}
    let prev_depth = min(min(pd00, pd10), min(pd01, pd11));
    {% endif %}
    // Compare two PREVIOUS-frame distances: where this surface point was
    // last frame (its world position through last frame's view-projection)
    // against what last frame's depth buffer holds there. Comparing against
    // this frame's distance instead would trip on every fast dolly. The
    // jitter does not touch z, so jittered and unjittered matrices agree.
    var d_expected = 1.0e30;
    if (!sky) {
        let ndc_now = vec2<f32>(uv_q.x * 2.0 - 1.0, 1.0 - uv_q.y * 2.0)
            + vec2<f32>(2.0 * jitter.x, -2.0 * jitter.y) / R;
        let world_h = cam.inv_view_proj * vec4<f32>(ndc_now, nearest_depth, 1.0);
        let prev_clip = cam.prev_view_proj * vec4<f32>(world_h.xyz / world_h.w, 1.0);
        d_expected = view_distance(prev_clip.z / prev_clip.w, cam.inv_proj);
    }
    let d_prev = view_distance(prev_depth, cam.inv_proj);
    // Fires only when something NEARER was there last frame, i.e. this
    // surface was hidden. When in doubt it rejects: a false disocclusion
    // costs one frame of aliasing, a missed one costs a ghost.
    let disoccluded = d_prev < d_expected * (1.0 - params.disocclusion_threshold);

    // ---- 4.6 Variance clip (G5). UNCONDITIONAL: wgsl_validation pins it. -
    // The box comes from the current frame's plain 3x3, in YCoCg of the
    // tonemapped colour, so one HDR outlier cannot open it. The history is
    // clipped along the line to the mean (not clamped per channel, which
    // lands on a box corner: a colour that may occur nowhere nearby).
    let mean = m1 / 9.0;
    let sigma = sqrt(max(m2 / 9.0 - mean * mean, vec3<f32>(0.0)));
    let gamma = mix(
        params.gamma_still,
        params.gamma_moving,
        clamp(speed_px / max(params.gamma_moving_speed, 1.0e-4), 0.0, 1.0),
    );
    let history_y = rgb_to_ycocg(history_rgb);
    let to_history = history_y - mean;
    // The floor is a few steps of the rgba16float history's quantisation
    // (2.4e-4 just below 1.0): below it a flat area would read its own
    // rounding as a clipped history and forget everything.
    let box_extent = max(gamma * sigma, vec3<f32>(1.5e-3));
    let over = abs(to_history) / box_extent;
    let clip_scale = max(max(over.x, max(over.y, over.z)), 1.0);
    let history_clipped = max(ycocg_to_rgb(mean + to_history / clip_scale), vec3<f32>(0.0));
    // |history - clipped| / |history - mean|, in [0, 1).
    let clip_amount = 1.0 - 1.0 / clip_scale;

    // ---- 4.8 Blend weight (G7) -------------------------------------------
    let has_history = params.reset < 0.5 && on_screen && !disoccluded;
    let n = min(n_prev, params.max_history);
    var alpha = 1.0 / (n + 1.0);
    alpha = max(alpha, params.motion_alpha * clamp(speed_px / max(params.motion_alpha_speed, 1.0e-4), 0.0, 1.0));
    alpha = max(alpha, reactive * params.reactive_valid);
    alpha = max(alpha, clip_amount * 0.5);
    if (params.upscaling > 0.5) {
        // No render sample near this output pixel this frame: lean on history.
        alpha *= mix(0.3, 1.0, confidence);
    }
    if (!has_history) {
        alpha = 1.0;
    }
    var result = mix(history_clipped, current, alpha);
    var n_next = min(n + 1.0, params.max_history) * (1.0 - clip_amount);
    if (!has_history) {
        n_next = 1.0;
    }

    // NaN / Inf guard: one bad value in the history would spread through the
    // clip box forever.
    let finite = all(result >= vec3<f32>(0.0)) && all(result <= vec3<f32>(1.0)) && n_next >= 0.0 && n_next <= 1024.0;
    if (!finite) {
        result = current;
        n_next = 0.0;
    }
    result = min(result, vec3<f32>(TONEMAPPED_MAX));

    // ---- 4.9 Outputs -----------------------------------------------------
    textureStore(history_curr_tex, q, vec4<f32>(result, n_next));

    var out_color = untonemap(result);
    let view = u32(params.debug_view + 0.5);
    if (view != 0u) {
        if (view == 1u) {
            // Velocity: hue = direction, brightness = length (1 output px /
            // frame = full). Sky in grey.
            let px = v * O;
            let len = clamp(length(px), 0.0, 1.0);
            let angle = atan2(px.y, px.x);
            let hue = 0.5 + 0.5 * cos(vec3<f32>(angle) + vec3<f32>(0.0, 2.0944, 4.1888));
            out_color = select(hue * len, vec3<f32>(0.18) + hue * len * 0.5, sky);
        } else if (view == 2u) {
            // Reprojection error: |current - unclipped history|.
            let e = abs(current - history_rgb);
            out_color = select(vec3<f32>(0.0), heat(max3(e) * 8.0), on_screen);
        } else if (view == 3u) {
            out_color = vec3<f32>(n_next / max(params.max_history, 1.0));
        } else if (view == 4u) {
            // R = off-screen reset, G = disocclusion, B = clip amount.
            out_color = vec3<f32>(
                select(1.0, 0.0, on_screen),
                select(0.0, 1.0, disoccluded),
                clip_amount,
            );
        } else if (view == 5u) {
            out_color = vec3<f32>(reactive * params.reactive_valid);
        } else if (view == 6u) {
            out_color = vec3<f32>(confidence);
        } else if (view == 8u) {
            out_color = untonemap(history_clipped);
        } else if (view == 9u) {
            out_color = untonemap(current);
        }
    }
    textureStore(out_tex, q, vec4<f32>(out_color, 1.0));
}
