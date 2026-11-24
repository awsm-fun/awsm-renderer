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
// Two colour spaces (see "Spaces" below): the history is STORED in the Karis
// map of linear HDR; everything that averages works in the display's own
// space (`to_work` / `from_work`).
//
// Every rule below exists because breaking it ghosts or blurs. The list is
// G1-G12 in docs/TAA.md; the tags in the comments refer to it.
//
// Apple GPU rules: the 3x3 neighbourhood is unrolled at template time (no
// loop, no per-invocation array), and no `switch` contains a `return`.

{% include "shared_wgsl/camera.wgsl" %}
{% include "shared_wgsl/taa/sky_velocity.wgsl" %}
{% include "shared_wgsl/taa/f16_round.wgsl" %}

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
    // History cap for a pixel moving at `max_history_moving_speed` or faster.
    max_history_moving: f32,
    max_history_moving_speed: f32,
    // Minimum current weight on a motion boundary, reached at
    // `motion_edge_speed` output px / frame of relative motion in the 3x3.
    motion_edge_alpha: f32,
    motion_edge_speed: f32,
    // Current weight per unit of clip amount.
    clip_alpha: f32,
    // The display's exposure multiplier (exp2 of the scene's EV), so the
    // working space below is the display's (G11).
    exposure: f32,
    _pad0: f32,
    _pad1: f32,
    // The unjittered view-projection of the frame BEFORE last (the camera
    // buffer's `prev_view_projection` is last frame's): a static surface's
    // own motion vector LAST frame, which last frame's velocity copy holds
    // wherever that surface was visible (the contact test, 4.7).
    prev_prev_view_projection: mat4x4<f32>,
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
// Last frame's motion vectors, at LAST frame's render size (a copy, like the
// previous depth): what moved where this surface's history comes from.
@group(0) @binding(10) var prev_velocity_tex: texture_2d<f32>;
{% if reactive %}
// Reactive mask, render size: 0 = trust history, 1 = ignore it (G8).
@group(0) @binding(11) var reactive_tex: texture_2d<f32>;
{% endif %}

{% if reactive %}
// The mask value of an opaque-path HUD pixel. HUD meshes shaded by the opaque
// pass are rasterised with the UNJITTERED camera under TAA and must not be
// accumulated (03 §5.4): the resolve passes them through, so they come out
// exactly as without TAA. (Forward HUD meshes are drawn after the resolve.)
const HUD_REACTIVE: f32 = 0.999;
{% endif %}

// ---- Spaces (G11) ----------------------------------------------------------
// Two spaces. The history is STORED in `tonemap` space (c / (1 + max), the
// Karis map of the linear HDR colour): exposure-independent, and its
// rgba16float steps stay fine up to very bright highlights, which bloom
// reads back from `out_tex`. Everything that AVERAGES (the reconstruction,
// the clip box, the blend) happens in the WORKING space: what the display
// will show, `display_curve(exposure * c)`. An anti-aliased pixel is the
// average of what is seen over its area, so averaging scene-linear colour
// and tonemapping afterwards leaves every high-contrast edge biased (a
// checker edge converged one to several 8-bit levels off). The display
// tonemapper also bounds highlights, which is what a Karis weight is for;
// with no tonemapper the working space keeps the Karis map.

// The largest tonemapped value the rgba16float history can hold below 1.0 is
// 1 - 2^-11; `untonemap` divides by (1 - max3), so nothing may reach 1.
const TONEMAPPED_MAX: f32 = 0.9995;

fn max3(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b));
}

fn min3(c: vec3<f32>) -> f32 {
    return min(c.r, min(c.g, c.b));
}

fn tonemap(c: vec3<f32>) -> vec3<f32> {
    return c / (1.0 + max3(c));
}

fn untonemap(t: vec3<f32>) -> vec3<f32> {
    return t / (1.0 - min(max3(t), TONEMAPPED_MAX));
}

{% match tonemapping %}
{% when ToneMapping::KhronosNeutralPbr %}
// Khronos PBR Neutral, exactly as the display pass applies it
// (display_wgsl/helpers/tonemap.wgsl), and its exact inverse.
const KN_START: f32 = 0.76;
const KN_D: f32 = 0.24;
const KN_DESATURATION: f32 = 0.15;
// Output stays below 1; the working space's upper bound.
const WORK_MAX: f32 = 1.0;

fn display_curve(color: vec3<f32>) -> vec3<f32> {
    let x = min3(color);
    let offset = select(0.04, x - 6.25 * x * x, x < 0.08);
    let result = color - vec3<f32>(offset);
    let peak = max3(result);
    if (peak < KN_START) {
        return result;
    }
    let new_peak = 1.0 - KN_D * KN_D / (peak + KN_D - KN_START);
    let g = 1.0 - 1.0 / (KN_DESATURATION * (peak - new_peak) + 1.0);
    return mix(result * (new_peak / peak), vec3<f32>(new_peak), g);
}

// max3 of the output is the compressed peak; it gives back the peak, then
// the desaturation weight, then the colour before compression. The offset's
// toe (x - 6.25 x^2 below 0.08) leaves min3 = 6.25 x^2 there and x - 0.04
// above, which inverts to the offset.
fn display_curve_inverse(y_in: vec3<f32>) -> vec3<f32> {
    let y = max(y_in, vec3<f32>(0.0));
    let new_peak = clamp(max3(y), KN_START, 0.99999988);
    let peak = KN_D * KN_D / (1.0 - new_peak) - KN_D + KN_START;
    let g = 1.0 - 1.0 / (KN_DESATURATION * (peak - new_peak) + 1.0);
    let uncompressed = (y - vec3<f32>(g * new_peak)) / (1.0 - g) * (peak / max(new_peak, 1.0e-6));
    let c = select(uncompressed, y, max3(y) < KN_START);
    let m = max(min3(c), 0.0);
    let x_toe = sqrt(m / 6.25);
    let offset = select(0.04, x_toe - m, m < 0.04);
    return max(c + vec3<f32>(offset), vec3<f32>(0.0));
}
{% when ToneMapping::Aces %}
// Narkowicz's ACES fit as the display pass applies it, WITHOUT its clamp to
// 1: the rational is monotonic with asymptote a / c, so it inverts exactly
// and keeps highlights above the display's white distinct (bloom reads
// them back). Below 1 it is the displayed value.
const ACES_A: f32 = 2.51;
const ACES_B: f32 = 0.03;
const ACES_C: f32 = 2.43;
const ACES_D: f32 = 0.59;
const ACES_E: f32 = 0.14;
const WORK_MAX: f32 = 1.0329;

fn display_curve(x: vec3<f32>) -> vec3<f32> {
    return x * (ACES_A * x + ACES_B) / (x * (ACES_C * x + ACES_D) + ACES_E);
}

// (a - c y) x^2 + (b - d y) x - e y = 0, the positive root, in the form
// without cancellation for each sign of (b - d y).
fn display_curve_inverse(y_in: vec3<f32>) -> vec3<f32> {
    let y = clamp(y_in, vec3<f32>(0.0), vec3<f32>(ACES_A / ACES_C - 1.0e-4));
    let qa = ACES_A - ACES_C * y;
    let qb = ACES_B - ACES_D * y;
    let root = sqrt(qb * qb + 4.0 * qa * ACES_E * y);
    let small = 2.0 * ACES_E * y / max(qb + root, vec3<f32>(1.0e-12));
    let large = (root - qb) / (2.0 * qa);
    return select(large, small, qb > vec3<f32>(0.0));
}
{% when _ %}
// No tonemapper: the display shows exposure * c (clipped at white), which
// is unbounded, so the working space keeps the Karis map of it.
const WORK_MAX: f32 = 1.0;

fn display_curve(c: vec3<f32>) -> vec3<f32> {
    return tonemap(c);
}

fn display_curve_inverse(t: vec3<f32>) -> vec3<f32> {
    return untonemap(t);
}
{% endmatch %}

// Linear HDR -> working space and back.
fn to_work(c: vec3<f32>) -> vec3<f32> {
    return display_curve(c * params.exposure);
}

fn from_work(w: vec3<f32>) -> vec3<f32> {
    return display_curve_inverse(w) / params.exposure;
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

// The motion part of the own-surface test (4.7): two of last frame's vectors
// belong to different surfaces when they differ by more than this many
// output px, or this fraction of this surface's own speed (rg16float
// precision and the small change of a surface's vector across a pixel).
const CONTACT_MOTION_PX: f32 = 0.25;
const CONTACT_MOTION_REL: f32 = 0.25;

// The velocity target's "behind last frame's camera" marker
// (shared_wgsl/motion_vector.wgsl): no usable vector.
fn is_marker(v: vec2<f32>) -> bool {
    return all(abs(v - vec2<f32>(2.0)) < vec2<f32>(1.0e-3));
}

// Positive view distance of a raw depth. Sky is "infinitely far".
//
// The camera is read straight from the uniform throughout (no
// `camera_from_raw` copy): the copy holds an array (`frustum_rays`) and every
// matrix, and the resolve needs a handful of fields.
fn view_distance(depth: f32) -> f32 {
    if (is_sky(depth)) {
        return 1.0e30;
    }
    // inv_proj * (0, 0, depth, 1): only the z and w rows of columns 2 and 3.
    let m = camera_raw.inv_proj;
    return -(m[2].z * depth + m[3].z) / (m[2].w * depth + m[3].w);
}

// Last frame's 2x2 footprint around unjittered position `uv`: the nearest
// and farthest raw depth and the texels holding them. Last frame's render
// pixel p held unjittered position p + 0.5 - prev_jitter, so the texel-space
// position is uv * size + prev_jitter - 0.5, in that texture's own size (the
// render size may have changed since).
struct Footprint {
    near: f32,
    far: f32,
    near_texel: vec2<i32>,
    far_texel: vec2<i32>,
}

fn prev_depth_footprint(uv: vec2<f32>, prev_jitter: vec2<f32>) -> Footprint {
    let dims = vec2<i32>(textureDimensions(prev_depth_tex));
    let hi = dims - 1;
    let pd0 = vec2<i32>(floor(uv * vec2<f32>(dims) + prev_jitter - 0.5));
    let c00 = clamp(pd0, vec2<i32>(0), hi);
    let c10 = clamp(pd0 + vec2<i32>(1, 0), vec2<i32>(0), hi);
    let c01 = clamp(pd0 + vec2<i32>(0, 1), vec2<i32>(0), hi);
    let c11 = clamp(pd0 + vec2<i32>(1, 1), vec2<i32>(0), hi);
    let pd00 = textureLoad(prev_depth_tex, c00, 0);
    let pd10 = textureLoad(prev_depth_tex, c10, 0);
    let pd01 = textureLoad(prev_depth_tex, c01, 0);
    let pd11 = textureLoad(prev_depth_tex, c11, 0);
    var f = Footprint(pd00, pd00, c00, c00);
    if (nearer(pd10, f.near)) { f.near = pd10; f.near_texel = c10; }
    if (nearer(f.far, pd10)) { f.far = pd10; f.far_texel = c10; }
    if (nearer(pd01, f.near)) { f.near = pd01; f.near_texel = c01; }
    if (nearer(f.far, pd01)) { f.far = pd01; f.far_texel = c01; }
    if (nearer(pd11, f.near)) { f.near = pd11; f.near_texel = c11; }
    if (nearer(f.far, pd11)) { f.far = pd11; f.far_texel = c11; }
    return f;
}

// Where the point seen now at unjittered `uv` with raw depth `depth` was
// last frame IF IT DID NOT MOVE: xy = its previous uv, z = its previous view
// distance. The jitter does not touch z, so the jittered inverse and the
// unjittered previous matrix agree on depth.
fn static_world(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let jitter = camera_raw.jitter.xy;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0)
        + vec2<f32>(2.0 * jitter.x, -2.0 * jitter.y) / camera_raw.viewport.zw;
    let world_h = camera_raw.inv_view_proj * vec4<f32>(ndc, depth, 1.0);
    return world_h.xyz / world_h.w;
}

fn clip_to_uv(clip: vec4<f32>) -> vec2<f32> {
    return vec2<f32>(clip.x / clip.w * 0.5 + 0.5, 0.5 - clip.y / clip.w * 0.5);
}

// The motion vector LAST frame (uv two frames ago -> uv last frame) of the
// static point seen now at unjittered `uv` with raw depth `depth`: what last
// frame's velocity target held wherever that point was visible, whatever the
// camera did since (it may have started, stopped or turned).
fn static_prev_motion(uv: vec2<f32>, depth: f32) -> vec2<f32> {
    let world = vec4<f32>(static_world(uv, depth), 1.0);
    return clip_to_uv(camera_raw.prev_view_projection * world)
        - clip_to_uv(params.prev_prev_view_projection * world);
}

fn static_reprojection(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let prev_clip = camera_raw.prev_view_projection * vec4<f32>(static_world(uv, depth), 1.0);
    let prev_ndc = prev_clip.xyz / prev_clip.w;
    return vec3<f32>(
        prev_ndc.x * 0.5 + 0.5,
        0.5 - prev_ndc.y * 0.5,
        view_distance(prev_ndc.z),
    );
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

{% if debug_views %}
fn heat(x: f32) -> vec3<f32> {
    let t = clamp(x, 0.0, 1.0);
    return clamp(
        vec3<f32>(1.5 - abs(4.0 * t - 3.0), 1.5 - abs(4.0 * t - 2.0), 1.5 - abs(4.0 * t - 1.0)),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}
{% endif %}

// ---- The 3x3 neighbourhoods of a workgroup, shared -----------------------
// An 8x8 block of output pixels reads at most a 10x10 block of render
// pixels (render size <= post size: TAA never supersamples), and each of
// those is in up to nine 3x3 neighbourhoods. Every render pixel of the block
// is loaded and moved to the working space ONCE, by one invocation, into
// workgroup memory: YCoCg of the working-space colour (the reconstruction
// and the clip box are both linear in it) and the raw depth.
const TILE: i32 = 10;
const TILE_TEXELS: u32 = 100u;
var<workgroup> tile: array<vec4<f32>, 100>;
{% if reactive %}
var<workgroup> tile_reactive: array<f32, 100>;
{% endif %}

fn tile_index(p: vec2<i32>, origin: vec2<i32>) -> u32 {
    let t = p - origin;
    return u32(t.y * TILE + t.x);
}

fn load_tile_texel(i: u32, origin: vec2<i32>, p_max: vec2<i32>) {
    let t = vec2<i32>(i32(i % u32(TILE)), i32(i / u32(TILE)));
    let p = clamp(origin + t, vec2<i32>(0), p_max);
    let c = to_work(sanitize(textureLoad(color_tex, p, 0).rgb));
    tile[i] = vec4<f32>(rgb_to_ycocg(c), textureLoad(depth_tex, p, 0));
    {% if reactive %}
    tile_reactive[i] = textureLoad(reactive_tex, p, 0).r;
    {% endif %}
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(local_invocation_index) lid: u32,
) {
    let post_dims = vec2<i32>(textureDimensions(out_tex));
    let render_dims = vec2<i32>(textureDimensions(color_tex));
    let R = vec2<f32>(render_dims);
    let O = vec2<f32>(post_dims);
    let p_max = render_dims - 1;
    let jitter = camera_raw.jitter.xy;

    // The render pixel the workgroup's first output pixel lands in (the same
    // expression as p0 below, so the two agree bit for bit), less the border.
    let q_first = vec2<i32>(wid.xy) * 8;
    let tile_origin = vec2<i32>(floor((vec2<f32>(q_first) + 0.5) / O * R)) - 1;
    // 100 texels, 64 invocations: two rounds (no loop: Apple GPU rules).
    load_tile_texel(lid, tile_origin, p_max);
    if (lid + 64u < TILE_TEXELS) {
        load_tile_texel(lid + 64u, tile_origin, p_max);
    }
    workgroupBarrier();

    let q = vec2<i32>(gid.xy);
    if (q.x >= post_dims.x || q.y >= post_dims.y) {
        return;
    }

    // ---- 4.1 Where this output pixel falls in the render image ----------
    let uv_q = (vec2<f32>(q) + 0.5) / O;
    let r_pos = uv_q * R;
    let p0 = clamp(vec2<i32>(floor(r_pos)), vec2<i32>(0), p_max);
    // Its 3x3 in the tile: the clamped image position of every tap is what
    // the tile holds at the unclamped one.
    let base = vec2<i32>(floor(r_pos)) - tile_origin;

    // The kernel is separable: exp(-k (dx^2 + dy^2)) = exp(-k dx^2) exp(-k dy^2).
    // A sample sits at p + 0.5 - j (G2: the jitter is removed here, where the
    // current frame is sampled, never in the vector).
    let dxy0 = vec2<f32>(clamp(p0 - 1, vec2<i32>(0), p_max)) + 0.5 - jitter - r_pos;
    let dxy1 = vec2<f32>(p0) + 0.5 - jitter - r_pos;
    let dxy2 = vec2<f32>(clamp(p0 + 1, vec2<i32>(0), p_max)) + 0.5 - jitter - r_pos;
    let k = params.kernel_sharpness;
    let w0 = exp(-k * dxy0 * dxy0);
    let w1 = exp(-k * dxy1 * dxy1);
    let w2 = exp(-k * dxy2 * dxy2);

    // ---- 4.2 / 4.3 / 4.6 One pass over the 3x3 render pixels ------------
    // Accumulates the reconstructed current colour (in YCoCg), the
    // unweighted YCoCg moments for the clip box, the nearest depth (whose
    // motion vector the pixel uses: dilation, G3) and the largest reactive
    // value.
    var current_y = vec3<f32>(0.0);
    var weight_sum = 0.0;
    var confidence = 0.0;
    var m1 = vec3<f32>(0.0);
    var m2 = vec3<f32>(0.0);
    let centre = tile[u32(base.y * TILE + base.x)];
    var nearest_depth = centre.w;
    var nearest_p = p0;
    var farthest_depth = nearest_depth;
    var farthest_p = p0;
    // The centre pixel's own depth and its four axis neighbours', for the
    // own-surface disocclusion test (4.7).
    var z_own = nearest_depth;
    var z_left = nearest_depth;
    var z_right = nearest_depth;
    var z_up = nearest_depth;
    var z_down = nearest_depth;
    var reactive = 0.0;
    {% if reactive %}
    // The centre's mask value (the HUD test below reuses it at native).
    var r_centre = 0.0;
    {% endif %}
    // Samples that count: all 9, minus opaque-path HUD pixels in the
    // reactive variant (the HUD is not part of the world's neighbourhood).
    var kept = 0.0;
    {% for dy in 0..3 %}
    {% for dx in 0..3 %}
    {
        // One tap of the unrolled 3x3 (weight w{{ dx }}.x * w{{ dy }}.y).
        let p = clamp(p0 + vec2<i32>({{ dx }} - 1, {{ dy }} - 1), vec2<i32>(0), p_max);
        let ti = u32((base.y + {{ dy }} - 1) * TILE + base.x + {{ dx }} - 1);
        {% if reactive %}
        // A mask value of exactly 1 marks a HUD pixel (see HUD_REACTIVE). It
        // must not reach its neighbours in any way: not through the reactive
        // max (an aliased halo), not through the reconstruction or the clip
        // box (a HUD-coloured fringe that the history would then keep).
        let r = tile_reactive[ti];
        let keep = select(1.0, 0.0, params.reactive_valid > 0.5 && r >= HUD_REACTIVE);
        {% if dx == 1 && dy == 1 %}r_centre = r;{% endif %}
        reactive = max(reactive, r * keep);
        {% else %}
        let keep = 1.0;
        {% endif %}
        let s = tile[ti];
        let y = s.xyz;
        let w = w{{ dx }}.x * w{{ dy }}.y * keep;
        current_y += y * w;
        weight_sum += w;
        confidence = max(confidence, w);
        m1 += y * keep;
        m2 += y * y * keep;
        kept += keep;
        let z = s.w;
        {% if dx == 0 && dy == 1 %}z_left = z;{% endif %}
        {% if dx == 2 && dy == 1 %}z_right = z;{% endif %}
        {% if dx == 1 && dy == 0 %}z_up = z;{% endif %}
        {% if dx == 1 && dy == 2 %}z_down = z;{% endif %}
        // HUD pixels stay out of the nearest / farthest search too: the scene
        // depth under a HUD pixel is the world behind it, but its vector is
        // the HUD's.
        if (keep > 0.5 && nearer(z, nearest_depth)) {
            nearest_depth = z;
            nearest_p = p;
        }
        if (keep > 0.5 && nearer(farthest_depth, z)) {
            farthest_depth = z;
            farthest_p = p;
        }
    }
    {% endfor %}
    {% endfor %}
    // No sample kept: every pixel of the 3x3 is HUD, and the pass-through
    // below replaces the result. Keep the arithmetic finite until then.
    if (weight_sum <= 0.0) {
        current_y = centre.xyz;
        weight_sum = 1.0;
        m1 = centre.xyz;
        m2 = m1 * m1;
        kept = 1.0;
    }
    var current = max(ycocg_to_rgb(current_y / weight_sum), vec3<f32>(0.0));

    // ---- 4.3 Motion vector of the nearest pixel (G1, G3) ----------------
    var v = textureLoad(velocity_tex, nearest_p, 0).xy;
    let sky = is_sky(nearest_depth);
    if (sky) {
        // No geometry anywhere in the 3x3: the vector is the camera's
        // rotation only.
        v = taa_sky_velocity(uv_q, camera_from_raw(camera_raw));
    }
    let speed_px = length(v * O);

    // Motion boundary: the nearest and the farthest surface of the 3x3 move
    // differently, i.e. this is the silhouette of something moving against
    // what is behind it. Such a pixel's coverage changes from frame to frame,
    // and its history holds the average coverage of the last frames (the
    // clip cannot help: both surfaces' colours are inside the box). A static
    // silhouette has no relative motion and keeps its full history.
    // (A flat 3x3 has one surface: the nearest's vector is the farthest's.)
    var v_far = v;
    if (any(farthest_p != nearest_p)) {
        v_far = textureLoad(velocity_tex, farthest_p, 0).xy;
    }
    if (is_sky(farthest_depth)) {
        v_far = taa_sky_velocity(uv_q, camera_from_raw(camera_raw));
    }
    let edge_motion = clamp(
        length((v - v_far) * O) / max(params.motion_edge_speed, 1.0e-4),
        0.0,
        1.0,
    );

    // ---- 4.5 History fetch (G4) ------------------------------------------
    let uv_hist = uv_q - v;
    let on_screen = all(uv_hist >= vec2<f32>(0.0)) && all(uv_hist <= vec2<f32>(1.0));
    let hist_texel = clamp(vec2<i32>(floor(uv_hist * O)), vec2<i32>(0), post_dims - 1);
    let n_prev = textureLoad(history_prev_tex, hist_texel, 0).a;
    // Fetched in the stored space, then moved to the working space.
    let history_rgb = to_work(untonemap(sample_history(uv_hist, O)));

    // ---- 4.7 Disocclusion (G6) -------------------------------------------
    // Both sides compare two PREVIOUS-frame distances: where a surface point
    // was last frame (through last frame's view-projection) against what last
    // frame's depth buffer holds where its history is fetched. Comparing
    // against this frame's distance would trip on every fast dolly. A test
    // fires only when something NEARER was there, i.e. the surface was
    // hidden. When in doubt it rejects: a false disocclusion costs a frame of
    // aliasing, a missed one costs a ghost.
    //
    // Test 1: the surface whose vector the pixel uses (the nearest of the
    // 3x3). Both sides are the nearest depth of their footprint, so a static
    // silhouette does not trip it.
    //
    // Undilated, the nearest IS the pixel's own surface (same depth), so
    // test 2's reprojection serves both.
    let dilated = any(nearest_p != p0);
    var own = vec3<f32>(0.0, 0.0, 1.0e30);
    if (!is_sky(z_own)) {
        own = static_reprojection(uv_q, z_own);
    }
    var d_expected = 1.0e30;
    if (!sky) {
        if (dilated) {
            d_expected = static_reprojection(uv_q, nearest_depth).z;
        } else {
            d_expected = own.z;
        }
    }
    let prev_footprint = prev_depth_footprint(uv_hist, camera_raw.jitter.zw);
    let d_prev = view_distance(prev_footprint.near);
    var disoccluded = d_prev < d_expected * (1.0 - params.disocclusion_threshold);
    {% if debug_views %}
    let diag_test1 = disoccluded;
    var diag_test2 = false;
    var diag_depth2 = false;
    var diag_mixed = false;
    var diag_covered = false;
    {% endif %}

    // Test 2: the pixel's OWN surface, with its own depth and its own vector.
    // Test 1 alone has two blind spots, both on the strip a moving object has
    // just left. A pixel next to the object takes the object's vector
    // (dilation), so test 1 compares the object with itself while the pixel
    // really shows what was behind it. And an object resting on the surface
    // behind it is within the relative slack of that surface near the contact
    // line. For a surface that did not move (its vector is the camera's
    // reprojection) the expected previous distance is exact, so there the
    // slack shrinks to the surface's own depth slope across a pixel.
    //
    // Away from any nearer surface this test takes the FARTHEST depth of the
    // previous footprint: it asks whether the pixel's own surface was visible
    // ANYWHERE in the 2x2 it came from. Taking the nearest would flag pixels
    // near a static silhouette whenever the jitter moved the footprint
    // across it.
    if (!is_sky(z_own)) {
        var v_own = v;
        if (dilated) {
            v_own = textureLoad(velocity_tex, p0, 0).xy;
        }
        let uv_hist_own = uv_q - v_own;
        let is_static = length((uv_hist_own - own.xy) * O) < 0.1;
        let d_own = view_distance(z_own);
        let slope = max(
            max(abs(view_distance(z_left) - d_own), abs(view_distance(z_right) - d_own)),
            max(abs(view_distance(z_up) - d_own), abs(view_distance(z_down) - d_own)),
        );
        // Twice the steepest one-pixel step (the footprint's samples are up
        // to a pixel away), plus a floor for depth precision. Never looser
        // than the general threshold.
        let slack_static = min(params.disocclusion_threshold, 2.0 * slope / max(d_own, 1.0e-6) + 0.002);
        let slack_own = select(params.disocclusion_threshold, slack_static, is_static);
        // The footprint this surface's history comes from: test 1's, unless
        // the two vectors differ by more than a quarter pixel (closer than
        // that, the 2x2 is all but the same, and four loads are saved).
        let relative_motion = length((v - v_own) * O) > 0.25;
        var fp_own = prev_footprint;
        if (relative_motion) {
            fp_own = prev_depth_footprint(uv_hist_own, camera_raw.jitter.zw);
        }
        var hidden = false;
        if (dilated && relative_motion) {
            // Beside a nearer surface that moves relative to this one, the
            // NEAREST of the footprint: the edge really did sweep across, so
            // any sample of the thing in front means it was there.
            hidden = view_distance(fp_own.near) < own.z * (1.0 - slack_own);
            {% if debug_views %}
            diag_depth2 = hidden;
            {% endif %}
        } else {
            // Away from a nearer surface moving relative to this one, the
            // FARTHEST of the footprint: was this surface visible anywhere in
            // the 2x2 (a static edge or a wire the jitter moved across the
            // footprint is not a reveal). Beside a nearer surface that does
            // not move relative to this one (or on a slope, where the nearest
            // of the 3x3 is the same surface one pixel nearer) depth is not
            // trusted at all: the jitter sweeps such edges across the
            // footprint every cycle, and resetting there makes thin geometry
            // shimmer. Only the motion test below runs there.
            let d_far = view_distance(fp_own.far);
            if (!dilated) {
                hidden = d_far < own.z * (1.0 - slack_own);
            }
            {% if debug_views %}
            diag_depth2 = hidden;
            {% endif %}
            if (!hidden && is_static) {
                // Motion, where depth cannot tell two surfaces apart: the
                // bottom of an object resting on this surface touches it in
                // depth. Last frame's vectors at the nearest and the farthest
                // texel of the footprint, each compared like with like:
                // - MIXED: the two differ (both are last frame's vectors):
                //   two surfaces moving differently shared the footprint, so
                //   the history here holds a blend of them;
                // - COVERED: everything in it was nearer than this surface,
                //   and what was at the farthest texel moved unlike this
                //   surface did LAST frame (its static motion through the
                //   previous two view-projections, so a camera that starts,
                //   stops or turns between the frames changes nothing).
                // Sky texels and texels without a vector (cleared to 0, or
                // the (2, 2) "behind last camera" marker) take no part: the
                // velocity target holds no motion for them.
                let pv_near = textureLoad(prev_velocity_tex, fp_own.near_texel, 0).xy;
                let pv_far = textureLoad(prev_velocity_tex, fp_own.far_texel, 0).xy;
                let near_valid = !is_sky(fp_own.near) && !is_marker(pv_near);
                let far_valid = !is_sky(fp_own.far) && !is_marker(pv_far);
                let motion_slack = max(CONTACT_MOTION_PX, CONTACT_MOTION_REL * length(v_own * O));
                let mixed = near_valid && far_valid
                    && length((pv_near - pv_far) * O) > motion_slack;
                var covered = false;
                if (far_valid && d_far < own.z) {
                    let v_own_prev = static_prev_motion(uv_q, z_own);
                    covered = length((pv_far - v_own_prev) * O) > motion_slack;
                }
                hidden = mixed || covered;
                {% if debug_views %}
                diag_mixed = mixed;
                diag_covered = covered;
                {% endif %}
            }
        }
        if (hidden) {
            disoccluded = true;
            {% if debug_views %}
            diag_test2 = true;
            {% endif %}
        }
    }

    // ---- 4.6 Variance clip (G5). UNCONDITIONAL: wgsl_validation pins it. -
    // The box comes from the current frame's plain 3x3, in YCoCg of the
    // working space (bounded: one HDR outlier cannot open it). The history is
    // clipped along the line to the mean (not clamped per channel, which
    // lands on a box corner: a colour that may occur nowhere nearby).
    let mean = m1 / kept;
    let sigma = sqrt(max(m2 / kept - mean * mean, vec3<f32>(0.0)));
    let gamma = mix(
        params.gamma_still,
        params.gamma_moving,
        clamp(speed_px / max(params.gamma_moving_speed, 1.0e-4), 0.0, 1.0),
    );
    let history_y = rgb_to_ycocg(history_rgb);
    let to_history = history_y - mean;
    // The floor: three steps of the STORED history's rgba16float
    // quantisation at the box mean, in working-space units, and at least
    // 1.5e-3, or a flat area reads its own rounding as a clipped history and
    // forgets everything. The three-step term depends on the colour: under
    // Khronos at exposure 1 it passes 1.5e-3 for grey between HDR ~0.6 and
    // ~0.85 (up to 2.1e-3 just below the shoulder, where the curve is still
    // linear and the Karis step is already coarse) and on bright saturated
    // colours (up to ~1e-2, through the desaturation); elsewhere it is below.
    let mean_linear = from_work(max(ycocg_to_rgb(mean), vec3<f32>(0.0)));
    let mean_peak = max(max3(mean_linear), 1.0e-6);
    let t_peak = max3(tonemap(mean_linear));
    // Three f16 steps (2^-11 of the value) of the stored peak, back in HDR.
    let t_up = min(t_peak * (1.0 + 3.0 * 4.8828125e-4), TONEMAPPED_MAX);
    let peak_up = t_up / (1.0 - t_up);
    let quant = max3(abs(to_work(mean_linear * (peak_up / mean_peak)) - ycocg_to_rgb(mean)));
    let box_extent = max(gamma * sigma, vec3<f32>(max(1.5e-3, quant)));
    let over = abs(to_history) / box_extent;
    let clip_scale = max(max(over.x, max(over.y, over.z)), 1.0);
    let history_clipped = max(ycocg_to_rgb(mean + to_history / clip_scale), vec3<f32>(0.0));
    // |history - clipped| / |history - mean|, in [0, 1).
    let clip_amount = 1.0 - 1.0 / clip_scale;

    // ---- 4.8 Blend weight (G7) -------------------------------------------
    let has_history = params.reset < 0.5 && on_screen && !disoccluded;
    // The cap shortens with speed: history resampled every frame softens a
    // little each time, and a long memory would carry that softness for a
    // quarter of a second after the motion stops.
    let history_cap = mix(
        params.max_history,
        min(params.max_history, params.max_history_moving),
        clamp(speed_px / max(params.max_history_moving_speed, 1.0e-4), 0.0, 1.0),
    );
    let n = min(n_prev, history_cap);
    var alpha = 1.0 / (n + 1.0);
    alpha = max(alpha, params.motion_edge_alpha * edge_motion);
    alpha = max(alpha, params.motion_alpha * clamp(speed_px / max(params.motion_alpha_speed, 1.0e-4), 0.0, 1.0));
    alpha = max(alpha, reactive * params.reactive_valid);
    alpha = max(alpha, clip_amount * params.clip_alpha);
    if (params.upscaling > 0.5) {
        // No render sample near this output pixel this frame: lean on history.
        alpha *= mix(0.3, 1.0, confidence);
    }
    if (!has_history) {
        alpha = 1.0;
    }
    var result = mix(history_clipped, current, alpha);
    var n_next = min(n + 1.0, history_cap) * (1.0 - clip_amount);
    if (!has_history) {
        n_next = 1.0;
    }

    {% if reactive %}
    // HUD pass-through. HUD pixels were rasterised unjittered, so their
    // samples sit at pixel centres: bilinear between the four nearest (exactly
    // the pixel itself at native resolution), no history, no reconstruction.
    let hud_pos = r_pos - 0.5;
    let hud_p = vec2<i32>(floor(hud_pos));
    let hud_f = hud_pos - vec2<f32>(hud_p);
    let h00 = clamp(hud_p, vec2<i32>(0), p_max);
    let h11 = clamp(hud_p + vec2<i32>(1, 1), vec2<i32>(0), p_max);
    let h10 = vec2<i32>(h11.x, h00.y);
    let h01 = vec2<i32>(h00.x, h11.y);
    let hud_nearest = select(h00, h11, hud_f >= vec2<f32>(0.5));
    // At native resolution the nearest is the centre tap, already loaded.
    var r_hud = r_centre;
    if (any(hud_nearest != p0)) {
        r_hud = textureLoad(reactive_tex, hud_nearest, 0).r;
    }
    let is_hud = params.reactive_valid > 0.5 && r_hud >= HUD_REACTIVE;
    if (is_hud) {
        let top = mix(
            to_work(sanitize(textureLoad(color_tex, h00, 0).rgb)),
            to_work(sanitize(textureLoad(color_tex, h10, 0).rgb)),
            hud_f.x,
        );
        let bottom = mix(
            to_work(sanitize(textureLoad(color_tex, h01, 0).rgb)),
            to_work(sanitize(textureLoad(color_tex, h11, 0).rgb)),
            hud_f.x,
        );
        result = mix(top, bottom, hud_f.y);
        n_next = 1.0;
    }
    {% endif %}

    // NaN / Inf guard: one bad value in the history would spread through the
    // clip box forever.
    // The blend of two working-space colours can land an ulp or two above
    // WORK_MAX (the display curve approaches it); only a real overflow or a
    // NaN (every comparison false) is "not finite". Then clamp back.
    let finite = all(result >= vec3<f32>(0.0)) && all(result <= vec3<f32>(WORK_MAX * 1.001)) && n_next >= 0.0 && n_next <= 1024.0;
    if (!finite) {
        result = current;
        n_next = 0.0;
    }
    result = min(result, vec3<f32>(WORK_MAX));
    // Back to linear HDR (out_tex) and to the stored space (the history).
    let result_linear = sanitize(from_work(result));
    let stored = min(tonemap(result_linear), vec3<f32>(TONEMAPPED_MAX));

    // ---- 4.9 Outputs -----------------------------------------------------
    // Both stores go through `taa_round_f16`: a truncating store would darken
    // the history a little more every frame (see f16_round.wgsl). The
    // history length is left as it is: a truncated `n` is at most one step
    // short and is rebuilt from the cap every frame, so it does not compound.
    textureStore(history_curr_tex, q, vec4<f32>(taa_round_f16(stored), n_next));

    {% if debug_views %}
    var out_color = result_linear;
    {% else %}
    let out_color = result_linear;
    {% endif %}
    // The debug views live in their own variant: the branch, even never
    // taken, cost about 0.12 ms at 1920x1435 (register pressure).
    {% if debug_views %}
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
            out_color = from_work(history_clipped);
        } else if (view == 9u) {
            out_color = from_work(current);
        } else if (view == 10u) {
            // Previous depth against where this surface was: 0.5 = the same
            // surface, darker = something nearer was there (disocclusion),
            // brighter = something farther.
            out_color = vec3<f32>(clamp(0.5 * d_prev / max(d_expected, 1.0e-6), 0.0, 1.0));
        } else if (view == 11u) {
            out_color = vec3<f32>(n_prev / max(params.max_history, 1.0));
        } else if (view == 12u) {
            out_color = vec3<f32>(alpha);
        } else if (view == 13u) {
            out_color = vec3<f32>(edge_motion);
        } else if (view == 14u) {
            // Diagnostics: R = dilated, G = test 1 fired, B = test 2 fired.
            out_color = vec3<f32>(
                select(0.0, 1.0, dilated),
                select(0.0, 1.0, diag_test1),
                select(0.0, 1.0, diag_test2),
            );
        } else if (view == 16u) {
            // Diagnostics, test 2's parts: R = depth, G = MIXED, B = COVERED.
            out_color = vec3<f32>(
                select(0.0, 1.0, diag_depth2),
                select(0.0, 1.0, diag_mixed),
                select(0.0, 1.0, diag_covered),
            );
        } else if (view == 15u) {
            // Diagnostics: R = final alpha >= 0.99, G = motion-edge term >= 0.99,
            // B = clip term >= 0.99.
            out_color = vec3<f32>(
                select(0.0, 1.0, alpha >= 0.99),
                select(0.0, 1.0, params.motion_edge_alpha * edge_motion >= 0.99),
                select(0.0, 1.0, clip_amount * params.clip_alpha >= 0.99),
            );
        }
    }
    {% endif %}
    textureStore(out_tex, q, vec4<f32>(taa_round_f16(out_color), 1.0));
}
