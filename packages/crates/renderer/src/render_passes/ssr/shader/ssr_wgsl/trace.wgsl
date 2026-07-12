// SSR trace — screen-space reflections (docs/plans/ssr.md).
//
// Production path: reflection via a view-space linear DDA march (the Hi-Z
// min-Z-pyramid accelerator was deleted; LinearDda is the production trace).
// Reconstruct the shaded pixel's view-space position + normal, reflect the
// view ray, march it against the scene depth buffer, and on a hit sample the
// HDR color there; Fresnel-weight + edge-fade it. On a MISS the ray falls
// back to the skybox cubemap along the world reflected direction (the
// reflected scene being off-screen doesn't mean there is no reflection), and
// edge/travel fades mix INTO that fallback rather than into black. The output
// is reflection-ONLY premultiplied color with alpha = coverage × travel (0
// only on sky/opt-out); the composite pass ADDITIVELY blends it over the HDR
// target — no read-modify-write hazard, and zero-coverage pixels are left
// untouched.
//
// The glossy / half_res template blocks are the structural permutation axes
// (§5a): each compiles ONLY into the variant that needs it, so Mirror carries
// none of the glossy/denoise code, etc. Temporal accumulation is NOT a trace
// axis anymore: history reprojection + neighborhood clamping live in the
// dedicated temporal pass (`ssr_wgsl/temporal.wgsl`) that runs AFTER the
// spatial resolve. The only temporal-aware piece left here is the RUNTIME
// per-frame jitter rotation, gated on `params.temporal_weight > 0.0`.

// CameraRaw + camera_from_raw (view / proj / inv_proj for reconstruction).
{% include "shared_wgsl/camera.wgsl" %}
// unpack_normal_tangent (octahedral world normal) → TBN, decode_octahedral, …
{% include "shared_wgsl/math.wgsl" %}

// Spread below this is a PERFECT MIRROR: the march is fully DETERMINISTIC
// (fixed 0.5 phase, no per-pixel IGN, no params.frame) and the resolve passes
// the pixel through unfiltered. The descriptor alpha is rgba8unorm (1/255 ≈
// 0.004), so 0.01 cleanly separates "authored spread 0" from real gloss.
// Keep in sync with resolve.wgsl's MIRROR_SPREAD_EPS.
const MIRROR_SPREAD_EPS: f32 = 0.01;

// ssr-spread-gate: the spread at which SSR's "near-mirror" treatment fully
// ramps out — here it scales the mirror-on-mirror env substitution (a hit
// surface's pre-composite color is missing energy only where brdf_pbr
// suppressed its IBL specular, i.e. under this same ramp). Keep in sync with
// the same-named constant in ssr_wgsl/resolve.wgsl and
// shared_wgsl/lighting/brdf_pbr.wgsl — grep "ssr-spread-gate".
const SSR_SPREAD_GATE: f32 = 0.15;
// ssr-spread-cutoff: the end of the SSR->IBL crossfade — the trace's output
// scales by the INVERSE of brdf_pbr's ssr_ibl_keep ramp so total reflection
// energy stays ~constant across the band (below the GATE: pure SSR with IBL
// specular suppressed; above this: pure IBL). Keep in sync with
// shared_wgsl/lighting/brdf_pbr.wgsl — grep "ssr-spread-cutoff".
const SSR_SPREAD_CUTOFF: f32 = 0.6;

// Live tuning uniforms — NOT permutation axes (§5a). 32 bytes / 8×f32.
struct SsrParams {
    intensity: f32,
    max_distance: f32,
    thickness: f32,
    max_steps: f32,       // integer step count, carried as f32 for packing
    spread_cutoff: f32,
    edge_fade: f32,
    temporal_weight: f32,
    frame: f32,     // monotonic; rotates the march jitter when temporal_weight > 0
};

// M1 probes everything with integer textureLoad (depth is non-filterable), so
// no sampler is bound yet; the glossy path (M2) adds a linear sampler for
// mip-prefiltered color reads.
@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var<uniform> params: SsrParams;
{% if multisampled_geometry %}
@group(0) @binding(2) var depth_tex: texture_depth_multisampled_2d;
@group(0) @binding(3) var normal_tangent_tex: texture_multisampled_2d<f32>;
{% else %}
@group(0) @binding(2) var depth_tex: texture_depth_2d;
@group(0) @binding(3) var normal_tangent_tex: texture_2d<f32>;
{% endif %}
// HDR color source is the RESOLVED single-sample `transparent` target even
// under MSAA, so it is never multisampled.
@group(0) @binding(4) var color_tex: texture_2d<f32>;
@group(0) @binding(5) var out_tex: texture_storage_2d<rgba16float, write>;
// M2a: material-owned reflection descriptor (single-sample, full-res). RGB =
// reflectivity color (ssr_mask * ssr_tint; 0 = surface opts out), A = ssr_spread
// (0 mirror … 1 diffuse). Written by `material_opaque`.
@group(0) @binding(6) var reflection_descriptor_tex: texture_2d<f32>;
{% if hzb %}
// Hi-Z pyramid (dual-extreme HZB): `.g` = the CLOSEST depth per tile, the
// conservative reflector bound the traversal tests spans against. Always
// entry 7 when present — the temporal history bindings moved to the dedicated
// temporal pass, so nothing else shifts it.
@group(0) @binding(7) var hzb_tex: texture_2d<f32>;
// Environment fallback (the same skybox cubemap + filtering sampler the
// material pass binds): sampled on a MISS so rays whose reflection is
// off-screen return the environment instead of black. Nested binding index —
// the skybox rides AFTER the hzb when gpu_culling is on, else takes its slot
// (same pattern the trace used for hzb-after-temporal historically).
@group(0) @binding(8) var skybox_tex: texture_cube<f32>;
@group(0) @binding(9) var skybox_sampler: sampler;
{% else %}
@group(0) @binding(7) var skybox_tex: texture_cube<f32>;
@group(0) @binding(8) var skybox_sampler: sampler;
{% endif %}

// Reconstruct VIEW-space position from a hardware depth sample at `uv`
// (forward-Z [0,1]). NDC y is flipped relative to UV.
fn view_pos_from_depth(uv: vec2<f32>, depth: f32, cam: Camera) -> vec3<f32> {
    let ndc = vec3<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth);
    let v = cam.inv_proj * vec4<f32>(ndc, 1.0);
    return v.xyz / v.w;
}

// Scene depth at a CONTINUOUS pixel position — bilinear over the 2x2 raw-depth
// neighborhood, falling back to the nearest point sample when any corner is
// sky (interpolating against the far plane would fabricate mid-air surfaces
// at silhouettes-vs-sky). THE fundamental fix for the quantization artifact
// family (wgsl_validation pins this): raw hardware depth is z_ndc, which
// interpolates EXACTLY linearly in screen space for planar surfaces, so
// point-sampling it makes every hit test binary at texel granularity —
// magnification then blows that staircase up into the dashed / striped /
// serrated reflections seen at every grazing contact and curved-silhouette
// apex. The depth buffer describes a continuous surface; sample it as one and
// intersections become continuous too — deterministically, with no need for
// stochastic supersampling. Fabricated fg/bg blends at interior depth
// DISCONTINUITIES are rejected by the guard below and by the post-refine
// penetration validation (a refined discontinuity keeps gap-sized
// penetration and fails the thickness bound).
fn scene_depth_at(pix: vec2<f32>, fdims: vec2<f32>) -> f32 {
    let p = pix - vec2<f32>(0.5);
    let base = floor(p);
    let f = p - base;
    let maxc = vec2<i32>(fdims) - vec2<i32>(1, 1);
    let zero = vec2<i32>(0, 0);
    let d00 = textureLoad(depth_tex, clamp(vec2<i32>(base), zero, maxc), 0);
    let d10 = textureLoad(depth_tex, clamp(vec2<i32>(base) + vec2<i32>(1, 0), zero, maxc), 0);
    let d01 = textureLoad(depth_tex, clamp(vec2<i32>(base) + vec2<i32>(0, 1), zero, maxc), 0);
    let d11 = textureLoad(depth_tex, clamp(vec2<i32>(base) + vec2<i32>(1, 1), zero, maxc), 0);
    {% if reverse_z %}
    let d_min = min(min(d00, d10), min(d01, d11));
    let d_max = max(max(d00, d10), max(d01, d11));
    let any_sky = d_min <= 0.0;
    {% else %}
    let d_min = min(min(d00, d10), min(d01, d11));
    let d_max = max(max(d00, d10), max(d01, d11));
    let any_sky = d_max >= 1.0;
    {% endif %}
    // DISCONTINUITY guard: when the 2x2 straddles a silhouette (large
    // relative raw-depth span), interpolating would fabricate a phantom
    // mid-air surface producing isolated speck false-hits floating around
    // every reflected silhouette.
    // Fall back to the point sample there — exactly the old behavior at
    // discontinuities, continuous everywhere it is meaningful. The 2% bound
    // passes grazing floors (per-texel raw delta well under 1%) and rejects
    // fg/bg jumps (tens of percent).
    if (any_sky || (d_max - d_min) > 0.02 * d_max) {
        return textureLoad(depth_tex, vec2<i32>(pix), 0);
    }
    return mix(mix(d00, d10, f.x), mix(d01, d11, f.x), f.y);
}

// Project a view-space position to screen UV ([0,1], y-down). Returns w<=0 in
// .z-sign convention via the caller checking clip.w through a sentinel: here we
// return uv and stash validity in the returned z (>0 valid).
fn view_to_uv(p_view: vec3<f32>, cam: Camera) -> vec3<f32> {
    let clip = cam.proj * vec4<f32>(p_view, 1.0);
    if (clip.w <= 0.0) {
        return vec3<f32>(0.0, 0.0, -1.0);
    }
    let ndc = clip.xyz / clip.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 1.0 - (ndc.y * 0.5 + 0.5));
    return vec3<f32>(uv, 1.0);
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_dims = textureDimensions(out_tex);
    let coords = vec2<i32>(gid.xy);
    if (coords.x >= i32(out_dims.x) || coords.y >= i32(out_dims.y)) {
        return;
    }
    // UV is resolution-independent, so full-res source loads work whether the
    // output is full- or half-res.
    let uv = (vec2<f32>(coords) + vec2<f32>(0.5)) / vec2<f32>(out_dims);
    // No level arg — valid for both `texture_depth_2d` and the multisampled form.
    let full_dims = textureDimensions(depth_tex);
    let fcoords = vec2<i32>(uv * vec2<f32>(full_dims));

    let cam = camera_from_raw(camera_raw);
    let depth = textureLoad(depth_tex, fcoords, 0);

    // Background / sky: nothing to reflect from. Reflection-only output → write
    // zero coverage so the additive composite leaves `composite` untouched.
    {% if reverse_z %}
    if (depth <= 0.0) {
    {% else %}
    if (depth >= 1.0) {
    {% endif %}
        textureStore(out_tex, coords, vec4<f32>(0.0, 0.0, 0.0, 0.0));
        return;
    }

    // M2a: material-owned reflectance. `reflectivity` folds mask*tint (0 = this
    // surface opts out of SSR entirely); `spread` is the reflection blur (0
    // mirror … 1 diffuse).
    let descriptor = textureLoad(reflection_descriptor_tex, fcoords, 0);
    let reflectivity = descriptor.rgb;
    let spread = descriptor.a;
    // Opt-out (non-reflective materials) OR too rough for a sharp mirror trace
    // (handed to IBL above `spread_cutoff`; the glossy path fills the gap in
    // M2b). Either way keep the base color untouched — zero SSR cost.
    let reflect_strength = max(reflectivity.r, max(reflectivity.g, reflectivity.b));
    if (reflect_strength < (1.0 / 255.0) || spread > params.spread_cutoff) {
        textureStore(out_tex, coords, vec4<f32>(0.0, 0.0, 0.0, 0.0));
        return;
    }

    let p = view_pos_from_depth(uv, depth, cam);
    let tbn = unpack_normal_tangent(textureLoad(normal_tangent_tex, fcoords, 0));
    let n = normalize((cam.view * vec4<f32>(tbn.N, 0.0)).xyz);
    let v = normalize(-p);
    let incident = normalize(p);
    var refl = reflect(incident, n);

    {% if glossy %}
    // M2: GGX importance-sample `refl` about the half-vector using the
    // material reflection descriptor's spread. (Mirror M1 uses the perfect
    // reflection above.)
    {% endif %}

    // ─── SCREEN-SPACE DDA march (McGuire & Mara style) ────────────────────
    //
    // The ray is marched in SCREEN PIXELS with perspective-correct depth
    // interpolation, not in view-space world units. This is the load-bearing
    // property: the stride can never exceed the ray's screen footprint, so a
    // thin reflector (a neon tube two pixels wide) is sampled by every march
    // that crosses it, near or far. The previous view-space marches (uniform,
    // then geometric) both under-sampled the far field — world strides grow
    // unboundedly in screen terms — which tore reflections into dashes and,
    // once jittered, into per-pixel sawtooth patches.

    // Interleaved-gradient-noise jitter of the stride PHASE, per pixel —
    // GLOSSY ONLY: decorrelates neighbouring pixels so residual coarse-stride
    // banding (only long rays stride > 1 px) turns into fine noise that the
    // resolve + temporal average away.
    let ign = fract(52.9829189 * fract(dot(vec2<f32>(coords), vec2<f32>(0.06711056, 0.00583715))));
    // When the temporal pass accumulates (temporal_weight > 0), rotate the
    // phase by the golden ratio each frame: the history blend averages the
    // march phase over ~1/(1-temporal_weight) frames and converges the noise.
    // RUNTIME gate (uniform read), not a template axis — a static pattern
    // suits the non-temporal path and the select costs nothing.
    let glossy_jitter = select(ign, fract(ign + params.frame * 0.61803398875), params.temporal_weight > 0.0);
    // MIRROR rays are fully DETERMINISTIC (wgsl_validation pins this
    // select): no per-pixel IGN, no per-frame dither. A stochastic mirror
    // turns every contact line into a per-pixel hit/miss lottery — and with
    // BILINEAR scene depth (see scene_depth_at) the intersection is already
    // continuous, so there is no texel-grid quantization left that would
    // need temporal supersampling. The fixed 0.5 phase centers each probe
    // in its stride.
    let jitter = select(glossy_jitter, 0.5, spread < MIRROR_SPREAD_EPS);

    // Cap the view-space ray: `max_distance`, and never through the camera
    // plane (a ray toward the camera clips so 1/w stays finite).
    // Normal-biased origin: nudge the start off the surface (scaled with
    // distance so the bias stays ~subpixel) so the contact-first 1 px probe
    // below never self-intersects the reflector's own surface. RETAINED for
    // now: it slightly distorts exact contacts, but removing it risks
    // reintroducing self-hit stipple — drop it only after on-device
    // verification of the mirror scene shows contacts stay clean without it.
    // No normal-biased origin: the bias (2cm+) made contact-grazing rays
    // start past the reflector's contact point and MISS — a visible gap
    // between a touching object and its reflection. Self-intersection is
    // guarded by the RELATIVE acceptance epsilon instead.
    let p_biased = p;
    var ray_len = params.max_distance;
    if (refl.z > 0.0) {
        ray_len = min(ray_len, max((-0.05 - p_biased.z) / refl.z, 0.0));
    }
    let p_end = p_biased + refl * ray_len;

    // Homogeneous endpoints; view-Z over w interpolates LINEARLY in screen
    // space (perspective-correct), so one lerp per step recovers exact ray
    // depth at each pixel.
    let fdims = vec2<f32>(full_dims);
    let h0 = cam.proj * vec4<f32>(p_biased, 1.0);
    let h1 = cam.proj * vec4<f32>(p_end, 1.0);
    let k0 = 1.0 / max(h0.w, 1e-6);
    let k1 = 1.0 / max(h1.w, 1e-6);
    let s0_center = vec2<f32>(
        (h0.x * k0 * 0.5 + 0.5) * fdims.x,
        (1.0 - (h0.y * k0 * 0.5 + 0.5)) * fdims.y,
    );
    let s1 = vec2<f32>(
        (h1.x * k1 * 0.5 + 0.5) * fdims.x,
        (1.0 - (h1.y * k1 * 0.5 + 0.5)) * fdims.y,
    );
    let qz0 = p_biased.z * k0;
    let qz1 = p_end.z * k1;

    let delta = s1 - s0_center;
    // Degenerate segment (ray ~along the view axis projects inside one
    // pixel): nothing new to sample along it — clamp so math stays finite;
    // the loop then exits on the first out-of-segment step.
    let screen_len = max(length(delta), 1e-3);
    let dir = delta / screen_len;
    let s0 = s0_center;
    let dk = (k1 - k0) / screen_len;
    let dqz = (qz1 - qz0) / screen_len;

    let steps = max(i32(params.max_steps), 1);
    // Stride covers the whole segment within the step budget, but never
    // finer than 1 px (sub-pixel probes are duplicates). Long rays stride
    // coarser; the jitter + binary refine recover the precision.
    let stride = max(screen_len / f32(steps), 1.0);

    var hit = false;
    var hit_uv = vec2<f32>(0.0, 0.0);
    {% if debug != 0 %}
    // Debug: iterations actually consumed by the march (both arms).
    var steps_used: f32 = 0.0;
    {% endif %}
    var travel_fade = 1.0;
    var travel_frac = 0.0;
    // Hit CONFIDENCE: ~1 at a clean refined surface crossing (penetration
    // near zero), fading to 0 as the refined penetration approaches the
    // leak threshold; blending by confidence turns marginal hits into a
    // smooth transition into the environment fallback.
    var hit_conf = 1.0;
    var s_prev = 0.0;
    // First probe at ~1 px UNCONDITIONALLY (first-probe-overshoot fix,
    // wgsl_validation pins this). The old start `stride * (0.5 + jitter)` is
    // 5-10 px into the ray on long marches (stride = screen_len / steps), so
    // contact geometry — the reflection meeting its reflector — was skipped
    // or hit stochastically, serrating every contact line into dark teeth.
    // The jitter phases the SECOND probe onward (see the advance below).
    var s_cur = 1.0;

{% if hzb %}
    // ─── Hi-Z traversal ────────────────────────────────────────────────────
    // Raw NDC depth ALSO interpolates linearly in screen space (z_clip/w),
    // so the coarse tests compare interpolated raw ray depth directly
    // against the pyramid's raw bounds — no per-cell linearization.
    let rz0 = h0.z * k0;
    let rz1 = h1.z * k1;
    let drz = (rz1 - rz0) / screen_len;

    let max_mip = i32(textureNumLevels(hzb_tex)) - 1;
    // Start at mip 0 (first-probe-overshoot fix): the first cells are
    // examined per-texel, so contact geometry right at the ray origin can
    // never be skipped. The re-ascent on every advance below coarsens the
    // traversal quickly across empty stretches, so the budget is unhurt.
    var mip = 0;
    // The iteration budget is the SAME `max_steps` knob: each iteration
    // either advances at least one cell or descends one mip, and empty
    // regions are skipped at coarse mips, so the budget goes much further
    // than the linear march's.
    for (var i = 0; i < steps; i = i + 1) {
        {% if debug != 0 %}
        steps_used = f32(i);
        {% endif %}
        if (s_cur >= screen_len) {
            break;
        }
        let pix = s0 + dir * s_cur;
        if (pix.x < 0.0 || pix.x >= fdims.x || pix.y < 0.0 || pix.y >= fdims.y) {
            break;
        }
        let cell_size = f32(1 << u32(mip));
        let cell = vec2<i32>(pix / cell_size);
        // Distance (along the ray, in pixels) to exit the current cell.
        var t_exit = screen_len;
        if (abs(dir.x) > 1e-6) {
            let bx = select(f32(cell.x) * cell_size, f32(cell.x + 1) * cell_size, dir.x > 0.0);
            t_exit = min(t_exit, (bx - s0.x) / dir.x);
        }
        if (abs(dir.y) > 1e-6) {
            let by = select(f32(cell.y) * cell_size, f32(cell.y + 1) * cell_size, dir.y > 0.0);
            t_exit = min(t_exit, (by - s0.y) / dir.y);
        }
        // Nudge past the boundary so the next iteration lands in the
        // neighbouring cell (never re-tests this one).
        let s_next = max(t_exit + 0.01, s_cur + 0.01);

        // Conservative span test against the cell's CLOSEST surface.
        let closest = textureLoad(hzb_tex, cell, mip).g;
        let rz_a = rz0 + drz * s_cur;
        let rz_b = rz0 + drz * min(s_next, screen_len);
        {% if reverse_z %}
        // Reverse-Z raw: closer = LARGER. A sky cell has closest == 0, which
        // no ray raw-depth (> 0) dips under — coarse sky skips are free.
        let possible = min(rz_a, rz_b) <= closest;
        {% else %}
        let possible = max(rz_a, rz_b) >= closest;
        {% endif %}

        if (!possible) {
            // Whole span provably in front of everything in the cell: skip
            // it and coarsen (the ray just crossed a cell boundary, so the
            // parent cell is fresh territory).
            s_prev = s_cur;
            s_cur = s_next;
            mip = min(mip + 1, max_mip);
            continue;
        }
        if (mip > 0) {
            // Possible hit somewhere in this cell — refine WITHOUT advancing.
            mip = mip - 1;
            continue;
        }

        // mip 0: exact per-texel test (same as the linear march).
        let sdepth = textureLoad(depth_tex, vec2<i32>(pix), 0);
        {% if reverse_z %}
        if (sdepth <= 0.0) {
        {% else %}
        if (sdepth >= 1.0) {
        {% endif %}
            s_prev = s_cur;
            s_cur = s_next;
            continue;
        }
        // Evaluation phase WITHIN the cell (entry -> exit). MIRROR pixels
        // evaluate mid-cell, deterministically — with bilinear scene depth
        // the penetration test is continuous, so no dither is needed. GLOSSY
        // pixels take glossy_jitter — per-pixel IGN (+ golden-ratio frame
        // rotation under temporal): in the Hi-Z path this is the only place
        // the glossy jitter can act (the cell crossings are geometric), so
        // without it the "stochastic" glossy trace would be deterministic
        // and its estimator error would freeze into static blotch.
        let eval_phase = select(glossy_jitter, 0.5, spread < MIRROR_SPREAD_EPS);
        let s_eval = mix(s_cur, min(s_next, screen_len), eval_phase);
        let s_prev_eval = mix(s_prev, s_cur, eval_phase);
        let k = k0 + dk * s_eval;
        let ray_z = -((qz0 + dqz * s_eval) / k);
        let ray_z_prev = -((qz0 + dqz * s_prev_eval) / (k0 + dk * s_prev_eval));
        // Ray and scene evaluated at the SAME continuous position (bilinear
        // depth — see scene_depth_at): penetration(s) is then continuous in
        // s and across neighbouring receiver pixels.
        let epix = s0 + dir * s_eval;
        let scene_z = -view_pos_from_depth(epix / fdims, scene_depth_at(epix, fdims), cam).z;
        let penetration = ray_z - scene_z;
        // CROSSING acceptance + post-refine validation (wgsl_validation pins
        // this — the angle-robust model). A hit is a SIGN CHANGE of
        // (ray_z - scene_z) across the step: in FRONT at the previous
        // sample, BEHIND now. With bilinear scene depth that difference is
        // CONTINUOUS, so the binary refine converges to an actual ROOT: at a
        // true surface crossing the refined penetration collapses toward
        // zero at ANY ray/surface steepness — the old absolute-thickness +
        // step-relative clause pair was angle-fragile (steep cameras
        // dissolved curved reflections into hit/miss stipple, and every
        // scene had to hand-tune ssr_thickness against it). A ray passing
        // BEHIND a thin/foreground object instead refines onto the depth
        // DISCONTINUITY and keeps a penetration ~ the whole gap, so one
        // absolute thickness bound rejects the leak-through and the march
        // CONTINUES past the occluder — thickness is now a leak threshold,
        // not a per-scene quality crutch.
        let ppix = s0 + dir * s_prev_eval;
        let pd = textureLoad(depth_tex, vec2<i32>(ppix), 0);
        {% if reverse_z %}
        let prev_sky = pd <= 0.0;
        {% else %}
        let prev_sky = pd >= 1.0;
        {% endif %}
        var front_prev = true;
        if (!prev_sky) {
            let pscene_z = -view_pos_from_depth(ppix / fdims, scene_depth_at(ppix, fdims), cam).z;
            front_prev = ray_z_prev < pscene_z * (1.0 + 1e-4);
        }
        if (!front_prev || penetration <= 1e-4 * scene_z) {
            // No crossing in this cell: advance one texel and RE-ASCEND.
            // Without the ascent the march stays at mip 0 forever after its
            // first descent and exhausts the iteration budget within ~steps
            // pixels — long reflections truncated on ray-direction-
            // dependent boundaries (the "non-round reflection" report).
            s_prev = s_cur;
            s_cur = s_next;
            mip = min(mip + 1, max_mip);
            continue;
        }
        {
            // Binary refine over the sign-change interval, then VALIDATE.
            var lo = s_prev_eval;
            var hi = s_eval;
            for (var b = 0; b < 8; b = b + 1) {
                let mid = 0.5 * (lo + hi);
                let mpix = s0 + dir * mid;
                let mk = k0 + dk * mid;
                let mray_z = -((qz0 + dqz * mid) / mk);
                let mscene_z = -view_pos_from_depth(mpix / fdims, scene_depth_at(mpix, fdims), cam).z;
                if (mray_z > mscene_z) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let rpix = s0 + dir * hi;
            let rray_z = -((qz0 + dqz * hi) / (k0 + dk * hi));
            let rscene_z = -view_pos_from_depth(rpix / fdims, scene_depth_at(rpix, fdims), cam).z;
            let pen_r = rray_z - rscene_z;
            let max_pen = params.thickness + 1e-3 * rscene_z;
            if (pen_r >= max_pen) {
                // Refined onto a depth DISCONTINUITY, not a root: the ray
                // passed BEHIND a foreground object. March on past it.
                s_prev = s_cur;
                s_cur = s_next;
                mip = min(mip + 1, max_mip);
                continue;
            }
            hit_conf = 1.0 - smoothstep(0.5, 1.0, max(pen_r, 0.0) / max_pen);
            hit_uv = rpix / fdims;
            travel_frac = hi / screen_len;
            travel_fade = 1.0 - smoothstep(0.7, 1.0, travel_frac);
            hit = true;
            break;
        }
    }
{% else %}
    for (var i = 0; i < steps; i = i + 1) {
        {% if debug != 0 %}
        steps_used = f32(i);
        {% endif %}
        if (s_cur >= screen_len) {
            break;
        }
        let pix = s0 + dir * s_cur;
        if (pix.x < 0.0 || pix.x >= fdims.x || pix.y < 0.0 || pix.y >= fdims.y) {
            break;
        }
        let sdepth = textureLoad(depth_tex, vec2<i32>(pix), 0);
        // Sky never occludes (and reverse-Z sky depth=0 would reconstruct to
        // a non-finite view position — skip it before the math).
        {% if reverse_z %}
        if (sdepth <= 0.0) {
        {% else %}
        if (sdepth >= 1.0) {
        {% endif %}
            s_prev = s_cur;
            // Same phased advance as the miss path below: probe i sits at
            // 1 + stride * (jitter + i - 1), so the deterministic-mirror
            // phase (0.5) and the glossy jitter both apply from probe 1 on.
            s_cur = 1.0 + stride * (jitter + f32(i));
            continue;
        }
        let k = k0 + dk * s_cur;
        let ray_z = -((qz0 + dqz * s_cur) / k);
        let ray_z_prev = -((qz0 + dqz * s_prev) / (k0 + dk * s_prev));
        // Bilinear scene depth (see scene_depth_at): continuous intersections.
        let scene_z = -view_pos_from_depth(pix / fdims, scene_depth_at(pix, fdims), cam).z;
        let penetration = ray_z - scene_z;
        // CROSSING acceptance + post-refine validation — same rationale as
        // the Hi-Z arm (see the comment there).
        let ppix = s0 + dir * s_prev;
        let pd = textureLoad(depth_tex, vec2<i32>(ppix), 0);
        {% if reverse_z %}
        let prev_sky = pd <= 0.0;
        {% else %}
        let prev_sky = pd >= 1.0;
        {% endif %}
        var front_prev = true;
        if (!prev_sky) {
            let pscene_z = -view_pos_from_depth(ppix / fdims, scene_depth_at(ppix, fdims), cam).z;
            front_prev = ray_z_prev < pscene_z * (1.0 + 1e-4);
        }
        if (front_prev && penetration > 1e-4 * scene_z) {
            // Binary refine over the sign-change interval, then VALIDATE.
            var lo = s_prev;
            var hi = s_cur;
            for (var b = 0; b < 8; b = b + 1) {
                let mid = 0.5 * (lo + hi);
                let mpix = s0 + dir * mid;
                let mk = k0 + dk * mid;
                let mray_z = -((qz0 + dqz * mid) / mk);
                let mscene_z = -view_pos_from_depth(mpix / fdims, scene_depth_at(mpix, fdims), cam).z;
                if (mray_z > mscene_z) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let rpix = s0 + dir * hi;
            let rray_z = -((qz0 + dqz * hi) / (k0 + dk * hi));
            let rscene_z = -view_pos_from_depth(rpix / fdims, scene_depth_at(rpix, fdims), cam).z;
            let pen_r = rray_z - rscene_z;
            let max_pen = params.thickness + 1e-3 * rscene_z;
            if (pen_r < max_pen) {
                hit_conf = 1.0 - smoothstep(0.5, 1.0, max(pen_r, 0.0) / max_pen);
                hit_uv = rpix / fdims;
                // Travel fade: reflections that reach the march budget must
                // not STOP on a hard line — fade the last 30% of the ray so
                // the termination boundary is invisible.
                travel_frac = hi / screen_len;
                travel_fade = 1.0 - smoothstep(0.7, 1.0, travel_frac);
                hit = true;
                break;
            }
            // Refined onto a discontinuity (pass-behind): march on past it.
        }
        s_prev = s_cur;
        // Phased advance: probe 0 sat at 1 px (contact-first); probe i (>= 1)
        // sits at 1 + stride * (jitter + i - 1). Mirror pixels use the fixed
        // 0.5 phase, glossy pixels the per-pixel (optionally frame-rotated)
        // jitter — see the `jitter` select above.
        s_cur = 1.0 + stride * (jitter + f32(i));
    }
{% endif %}

    // Schlick Fresnel with the material's specular F0 (vec3): dielectrics
    // (F0≈0.04) are weak at normal incidence and ramp to white at grazing;
    // metals (F0=base color) reflect strongly and tinted at all angles.
    // Computed for hit AND miss — the environment fallback below is
    // Fresnel-weighted exactly like a screen-space hit.
    let f0 = reflectivity;
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - max(dot(n, v), 0.0), 5.0);

    // ENVIRONMENT FALLBACK (wgsl_validation pins the skybox sample): a MISS —
    // the ray left the screen, exhausted its budget, or crossed only sky —
    // means the reflected scene is OFF-SCREEN, not absent. Sample the skybox
    // cubemap along the WORLD reflected direction so those rays return the
    // environment instead of black. Sky texels skipped during the march land
    // here too and sample the same sky — consistent. The material's IBL
    // specular is suppressed while the SSR descriptor is written (see
    // brdf_pbr.wgsl's ssr-spread-gate), so SSR owns the WHOLE reflection:
    // geometry on a hit, environment on a miss — no double counting.
    let dir_w = normalize((cam.inv_view * vec4<f32>(refl, 0.0)).xyz);
    // Spread-scaled mip of the PREFILTERED env (same `roughness * max_mip`
    // convention as samplePrefilteredEnv — wgsl_validation pins this): the
    // fallback replaces the IBL specular term the brdf suppressed, so it must
    // blur with the material exactly like IBL would. Sampling mip 0
    // unconditionally turned every star of a starfield skybox into a bright
    // blob reflection on near-mirror floors.
    let env_mip = spread * f32(textureNumLevels(skybox_tex) - 1u);
    let env = textureSampleLevel(skybox_tex, skybox_sampler, dir_w, env_mip).rgb;
    let env_reflection = env * fresnel * params.intensity;

    // Alpha carries coverage × travel for the resolve's distance-scaled blur.
    // Env-fallback misses store 1.0 (max travel → max glossy blur; mirror
    // pixels bypass the resolve anyway).
    var reflection = env_reflection;
    var coverage = 1.0;
    {% if debug == 1 %}
    var debug_hit_blend: f32 = 0.0;
    {% endif %}
    if (hit) {
        let hc = vec2<i32>(hit_uv * vec2<f32>(full_dims));
        var hit_color: vec3<f32>;
        {% if glossy %}
        // M2b glossy: approximate the GGX reflection cone by averaging a small
        // screen-space disk of the reflected color, radius ∝ `spread` (roughness).
        // spread 0 → single tap (perfect mirror); rougher → wider, softer blur.
        // A golden-angle spiral spreads the 8 taps evenly. (A future refinement is
        // a prefiltered color mip pyramid + stochastic sampling + temporal reuse.)
        // Cone radius GROWS WITH TRAVEL (wgsl_validation pins this): a rough
        // reflection is a cone footprint, so its blur ∝ distance travelled —
        // contact reflections sharpen exactly like contact shadows do. A
        // travel-independent radius blurred the contact zone with far-field
        // width, smearing the magnified contact structure into visible arc
        // banding. Full cone by ~12% of screen height of travel; small floor
        // keeps the estimator from collapsing to one noisy tap.
        let screen_travel = length(hit_uv * fdims - vec2<f32>(fcoords));
        // Cone floor 0.3 (was 0.08): contact reflections of thin BRIGHT
        // features on a near-mirror floor were sharp enough to expose the
        // view-dependent quantization pattern, which CRAWLS as the camera
        // moves — far more distracting than a slightly-soft contact. A
        // near-contact glossy reflection now keeps ~1/3 of the far-field
        // blur (still sharpening toward contact, like contact shadows, just
        // never to raw-texel sharpness).
        let cone = clamp(screen_travel / (0.12 * f32(full_dims.y)), 0.3, 1.0);
        let blur_radius = spread * f32(full_dims.y) * 0.045 * cone;
        if (blur_radius < 0.75) {
            // SHARP path (mirror + near-mirror): bilinear reconstruction at
            // the refined sub-texel hit (wgsl_validation pins this). Nearest
            // sampling at the quantized hit texel breaks thin source
            // features — an object's 1px antialiased rim over the dark
            // pre-SSR floor — into dotted serration when the reflection
            // magnifies them; the binary refine already produces a
            // sub-texel hit_uv, so a manual 2x2 bilinear reconstructs them
            // as the smooth curves the object itself shows. Max footprint =
            // 1 texel: reconstruction, not blur.
            let hpos = hit_uv * vec2<f32>(full_dims) - vec2<f32>(0.5);
            let hbase = floor(hpos);
            let hfrac = hpos - hbase;
            let hmax = vec2<i32>(full_dims) - vec2<i32>(1, 1);
            let hzero = vec2<i32>(0, 0);
            let h00 = textureLoad(color_tex, clamp(vec2<i32>(hbase), hzero, hmax), 0).rgb;
            let h10 = textureLoad(color_tex, clamp(vec2<i32>(hbase) + vec2<i32>(1, 0), hzero, hmax), 0).rgb;
            let h01 = textureLoad(color_tex, clamp(vec2<i32>(hbase) + vec2<i32>(0, 1), hzero, hmax), 0).rgb;
            let h11 = textureLoad(color_tex, clamp(vec2<i32>(hbase) + vec2<i32>(1, 1), hzero, hmax), 0).rgb;
            hit_color = mix(mix(h00, h10, hfrac.x), mix(h01, h11, hfrac.x), hfrac.y);
        } else {
            var acc = vec3<f32>(0.0);
            // Rotate the whole disk per PIXEL (and per frame when temporal
            // accumulates — glossy_jitter carries both) — a fixed spiral
            // gives neighbouring pixels the SAME sparse 8-tap pattern, so
            // their estimator errors correlate into static blotch that no
            // downstream average can remove; rotation decorrelates them and
            // the resolve's 9-tap + temporal then see independent estimates.
            let disk_rot = glossy_jitter * 6.28318530718;
            for (var s = 0; s < 8; s = s + 1) {
                let fs = f32(s);
                let ang = fs * 2.3999632 + disk_rot; // golden angle (radians)
                let rad = blur_radius * sqrt((fs + 0.5) / 8.0);
                let off = vec2<f32>(cos(ang), sin(ang)) * rad;
                let sc = clamp(
                    vec2<i32>(vec2<f32>(hc) + off),
                    vec2<i32>(0, 0),
                    vec2<i32>(full_dims) - vec2<i32>(1, 1),
                );
                acc = acc + textureLoad(color_tex, sc, 0).rgb;
            }
            hit_color = acc / 8.0;
        }
        {% else %}
        // MIRROR: bilinear reconstruction at the refined sub-texel hit
        // (wgsl_validation pins this). Nearest sampling (textureLoad at the
        // quantized hit texel) breaks thin source features — an object's
        // 1px antialiased rim over the dark pre-SSR floor — into dotted
        // serration when the reflection magnifies them; the binary refine
        // already produces a sub-texel hit_uv, so a manual 2x2 bilinear
        // reconstructs those features as the smooth curves the object
        // itself shows. Max footprint = 1 texel: reconstruction, not blur.
        let hpos = hit_uv * vec2<f32>(full_dims) - vec2<f32>(0.5);
        let hbase = floor(hpos);
        let hfrac = hpos - hbase;
        let hmax = vec2<i32>(full_dims) - vec2<i32>(1, 1);
        let hzero = vec2<i32>(0, 0);
        let h00 = textureLoad(color_tex, clamp(vec2<i32>(hbase), hzero, hmax), 0).rgb;
        let h10 = textureLoad(color_tex, clamp(vec2<i32>(hbase) + vec2<i32>(1, 0), hzero, hmax), 0).rgb;
        let h01 = textureLoad(color_tex, clamp(vec2<i32>(hbase) + vec2<i32>(0, 1), hzero, hmax), 0).rgb;
        let h11 = textureLoad(color_tex, clamp(vec2<i32>(hbase) + vec2<i32>(1, 1), hzero, hmax), 0).rgb;
        hit_color = mix(mix(h00, h10, hfrac.x), mix(h01, h11, hfrac.x), hfrac.y);
        {% endif %}
        {% if glossy %}
        // GLOSSY HDR clamp (wgsl_validation pins this): a bloom-hot emissive
        // (10x+ HDR) reflected near contact keeps visible banding through
        // ANY practical blur width, and the banding pattern is
        // view-dependent so it CRAWLS as the camera moves. Clamping the hit
        // luminance before filtering tames the contrast the same way TAA's
        // Karis weighting tames fireflies — the reflection reads slightly
        // dimmer (the primary surface's own bloom still glows), the blur
        // finally fuses, and the crawl disappears. Mirrors (the non-glossy
        // template) stay exact.
        let hit_lum = max(hit_color.r, max(hit_color.g, hit_color.b));
        hit_color = hit_color * min(1.0, 3.0 / max(hit_lum, 1e-4));
        {% endif %}
        // MIRROR-ON-MIRROR fallback (wgsl_validation pins this): a hit ON a
        // reflective surface samples that surface's PRE-composite color,
        // which may be missing its own reflected energy — a metallic
        // mirror's post-opaque color is near-black, so reflections of
        // reflectors read as dark speckles/dashes exactly where skimming
        // rays land on the reflector visible behind an object's silhouette.
        // True multi-bounce is out of scope for single-pass SSR; substitute
        // the ENVIRONMENT (what a further bounce converges to for an
        // unoccluded mirror) — but ONLY in proportion to the energy the
        // pre-composite buffer actually LACKS: brdf_pbr suppresses IBL
        // specular only under the ssr-spread-gate ramp, so a NEAR-MIRROR
        // hit is missing its reflection while a rough reflector (a brushed-
        // metal sphere) is complete and must never be swapped for sky —
        // substituting by reflectivity alone ERASED rough metals' own
        // reflections from every mirror.
        let hit_desc = textureLoad(reflection_descriptor_tex, hc, 0);
        let hit_reflectivity = max(hit_desc.r, max(hit_desc.g, hit_desc.b));
        let hit_missing = hit_reflectivity
            * (1.0 - smoothstep(0.0, SSR_SPREAD_GATE, hit_desc.a));
        hit_color = mix(hit_color, env, hit_missing);
        // Fade toward the screen borders of the hit to hide the SS seam —
        // and fade INTO the env fallback, not into black (wgsl_validation
        // pins the mix): an edge-faded or budget-faded (travel_fade) hit
        // transitions to the same environment a miss one pixel over returns.
        let edge = min(min(hit_uv.x, 1.0 - hit_uv.x), min(hit_uv.y, 1.0 - hit_uv.y));
        let fade = smoothstep(0.0, max(params.edge_fade, 1e-4), edge);
        let hit_reflection = hit_color * fresnel * params.intensity;
        reflection = mix(env_reflection, hit_reflection, fade * travel_fade * hit_conf);
        {% if debug == 1 %}
        debug_hit_blend = fade * travel_fade * hit_conf;
        {% endif %}
        coverage = max(travel_frac, 0.05);
    }

    {% if half_res %}
    // Half-res trace: the guided upsample runs in the composite step.
    {% endif %}

    // Alpha = COVERAGE × TRAVEL: sky/opt-out pixels wrote 0 and returned
    // above; a hit stores its travel fraction (never below 0.05 so a contact
    // hit still reads as covered); an env-fallback miss stores 1.0. The
    // additive composite only consumes rgb; the spatial resolve reads this
    // to scale its blur with reflection distance (contact-sharp,
    // distance-soft — also what buries the serrated edges of mirror-sharp
    // thin-tube reflections).

    // SSR->IBL CROSSFADE (wgsl_validation pins this): scale by the inverse
    // of brdf_pbr's ssr_ibl_keep ramp — as the material's IBL specular fades
    // back in across [GATE, CUTOFF], SSR bows out complementarily. Without
    // this the mid-gloss band double-counted reflection energy.
    let ssr_own = 1.0 - smoothstep(SSR_SPREAD_GATE, SSR_SPREAD_CUTOFF, spread);
    {% if debug == 0 %}
    // Reflection-ONLY, premultiplied. Full-res invariant: composite_old +
    // reflection == the old base + reflection overwrite, since composite_old
    // == base at this pixel.
    textureStore(out_tex, coords, vec4<f32>(reflection * ssr_own, coverage));
    {% else %}
    // DEBUG VISUALIZATION (wgsl_validation pins the encodings): the debug
    // axis REPLACES the reflection with an encoded value. The composite is
    // additive, so encodings are bright enough to dominate the (dark) scene;
    // read them on dark content or with bloom off.
    var dbg = vec3<f32>(0.0);
    {% if debug == 1 %}
    // CONFIDENCE: green = the hit's blend factor (fade x travel_fade x
    // hit_conf); red = env fallback (no hit); black = SSR-inactive pixel.
    if (hit) {
        dbg = vec3<f32>(0.0, debug_hit_blend, 0.0);
    } else {
        dbg = vec3<f32>(0.6, 0.0, 0.0);
    }
    {% else if debug == 2 %}
    // TRAVEL: heat ramp of travel_frac on hits (blue near -> red far).
    if (hit) {
        dbg = mix(vec3<f32>(0.0, 0.2, 1.0), vec3<f32>(1.0, 0.1, 0.0), travel_frac);
    }
    {% else if debug == 3 %}
    // SOURCE: green = screen-space hit, blue = env fallback, black = none.
    if (hit) {
        dbg = vec3<f32>(0.0, 0.8, 0.0);
    } else {
        dbg = vec3<f32>(0.0, 0.1, 0.9);
    }
    {% else if debug == 4 %}
    // STEPS: gray ramp of iterations / max_steps (white = budget exhausted).
    dbg = vec3<f32>(steps_used / max(params.max_steps, 1.0));
    {% endif %}
    textureStore(out_tex, coords, vec4<f32>(dbg, 1.0));
    {% endif %}
}
