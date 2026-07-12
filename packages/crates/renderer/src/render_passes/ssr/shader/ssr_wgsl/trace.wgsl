// SSR trace — screen-space reflections (docs/plans/ssr.md).
//
// Production path: reflection via a view-space linear DDA march (the Hi-Z
// min-Z-pyramid accelerator was deleted; LinearDda is the production trace).
// Reconstruct the shaded pixel's view-space position + normal, reflect the
// view ray, march it against the scene depth buffer, and on a hit sample the
// HDR color there; Fresnel-weight + edge-fade it. The output is
// reflection-ONLY premultiplied color with alpha = coverage (1 on a hit, 0 on
// miss/sky/opt-out); the composite pass ADDITIVELY blends it over the HDR
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
// conservative reflector bound the traversal tests spans against. Always the
// last (7th) entry — the temporal history bindings moved to the dedicated
// temporal pass, so the trace layout is fixed.
@group(0) @binding(7) var hzb_tex: texture_2d<f32>;
{% endif %}

// Reconstruct VIEW-space position from a hardware depth sample at `uv`
// (forward-Z [0,1]). NDC y is flipped relative to UV.
fn view_pos_from_depth(uv: vec2<f32>, depth: f32, cam: Camera) -> vec3<f32> {
    let ndc = vec3<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth);
    let v = cam.inv_proj * vec4<f32>(ndc, 1.0);
    return v.xyz / v.w;
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

    // Interleaved-gradient-noise jitter of the stride PHASE, per pixel:
    // decorrelates neighbouring pixels so residual coarse-stride banding
    // (only long rays stride > 1 px) turns into fine noise.
    let ign = fract(52.9829189 * fract(dot(vec2<f32>(coords), vec2<f32>(0.06711056, 0.00583715))));
    // When the temporal pass accumulates (temporal_weight > 0), rotate the
    // phase by the golden ratio each frame: the history blend averages the
    // march phase over ~1/(1-temporal_weight) frames and converges the noise.
    // RUNTIME gate (uniform read), not a template axis — a static pattern
    // suits the non-temporal path and the select costs nothing.
    let jitter = select(ign, fract(ign + params.frame * 0.61803398875), params.temporal_weight > 0.0);

    // Cap the view-space ray: `max_distance`, and never through the camera
    // plane (a ray toward the camera clips so 1/w stays finite).
    var ray_len = params.max_distance;
    if (refl.z > 0.0) {
        ray_len = min(ray_len, max((-0.05 - p.z) / refl.z, 0.0));
    }
    let p_end = p + refl * ray_len;

    // Homogeneous endpoints; view-Z over w interpolates LINEARLY in screen
    // space (perspective-correct), so one lerp per step recovers exact ray
    // depth at each pixel.
    let fdims = vec2<f32>(full_dims);
    let h0 = cam.proj * vec4<f32>(p, 1.0);
    let h1 = cam.proj * vec4<f32>(p_end, 1.0);
    let k0 = 1.0 / max(h0.w, 1e-6);
    let k1 = 1.0 / max(h1.w, 1e-6);
    let s0 = vec2<f32>(
        (h0.x * k0 * 0.5 + 0.5) * fdims.x,
        (1.0 - (h0.y * k0 * 0.5 + 0.5)) * fdims.y,
    );
    let s1 = vec2<f32>(
        (h1.x * k1 * 0.5 + 0.5) * fdims.x,
        (1.0 - (h1.y * k1 * 0.5 + 0.5)) * fdims.y,
    );
    let qz0 = p.z * k0;
    let qz1 = p_end.z * k1;

    let delta = s1 - s0;
    // Degenerate segment (ray ~along the view axis projects inside one
    // pixel): nothing new to sample along it — clamp so math stays finite;
    // the loop then exits on the first out-of-segment step.
    let screen_len = max(length(delta), 1e-3);
    let dir = delta / screen_len;
    let dk = (k1 - k0) / screen_len;
    let dqz = (qz1 - qz0) / screen_len;

    let steps = max(i32(params.max_steps), 1);
    // Stride covers the whole segment within the step budget, but never
    // finer than 1 px (sub-pixel probes are duplicates). Long rays stride
    // coarser; the jitter + binary refine recover the precision.
    let stride = max(screen_len / f32(steps), 1.0);

    var hit = false;
    var hit_uv = vec2<f32>(0.0, 0.0);
    var travel_fade = 1.0;
    var s_prev = 0.0;
    var s_cur = stride * (0.5 + jitter);

{% if hzb %}
    // ─── Hi-Z traversal ────────────────────────────────────────────────────
    // Raw NDC depth ALSO interpolates linearly in screen space (z_clip/w),
    // so the coarse tests compare interpolated raw ray depth directly
    // against the pyramid's raw bounds — no per-cell linearization.
    let rz0 = h0.z * k0;
    let rz1 = h1.z * k1;
    let drz = (rz1 - rz0) / screen_len;

    let max_mip = i32(textureNumLevels(hzb_tex)) - 1;
    var mip = 1;
    // The iteration budget is the SAME `max_steps` knob: each iteration
    // either advances at least one cell or descends one mip, and empty
    // regions are skipped at coarse mips, so the budget goes much further
    // than the linear march's.
    for (var i = 0; i < steps; i = i + 1) {
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
        let k = k0 + dk * s_cur;
        let ray_z = -((qz0 + dqz * s_cur) / k);
        let scene_z = -view_pos_from_depth(pix / fdims, sdepth, cam).z;
        let thickness = max(params.thickness, scene_z * 0.02);
        if (ray_z > scene_z + 0.01 && (ray_z - scene_z) < thickness) {
            var lo = s_prev;
            var hi = s_cur;
            for (var b = 0; b < 5; b = b + 1) {
                let mid = 0.5 * (lo + hi);
                let mpix = s0 + dir * mid;
                let md = textureLoad(depth_tex, vec2<i32>(mpix), 0);
                let mk = k0 + dk * mid;
                let mray_z = -((qz0 + dqz * mid) / mk);
                let mscene_z = -view_pos_from_depth(mpix / fdims, md, cam).z;
                if (mray_z > mscene_z) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            hit_uv = (s0 + dir * hi) / fdims;
            travel_fade = 1.0 - smoothstep(0.7, 1.0, hi / screen_len);
            hit = true;
            break;
        }
        s_prev = s_cur;
        s_cur = s_next;
    }
{% else %}
    for (var i = 0; i < steps; i = i + 1) {
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
            s_cur = s_cur + stride;
            continue;
        }
        let k = k0 + dk * s_cur;
        let ray_z = -((qz0 + dqz * s_cur) / k);
        let scene_z = -view_pos_from_depth(pix / fdims, sdepth, cam).z;
        // Thickness: the base tolerance, widened proportionally with depth
        // (2%) so grazing far-field surfaces don't tunnel between texels.
        let thickness = max(params.thickness, scene_z * 0.02);
        if (ray_z > scene_z + 0.01 && (ray_z - scene_z) < thickness) {
            // Binary refinement over the last screen-space interval.
            var lo = s_prev;
            var hi = s_cur;
            for (var b = 0; b < 5; b = b + 1) {
                let mid = 0.5 * (lo + hi);
                let mpix = s0 + dir * mid;
                let md = textureLoad(depth_tex, vec2<i32>(mpix), 0);
                let mk = k0 + dk * mid;
                let mray_z = -((qz0 + dqz * mid) / mk);
                let mscene_z = -view_pos_from_depth(mpix / fdims, md, cam).z;
                if (mray_z > mscene_z) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            hit_uv = (s0 + dir * hi) / fdims;
            // Travel fade: reflections that reach the march budget must not
            // STOP on a hard line — fade the last 30% of the ray so the
            // termination boundary is invisible.
            travel_fade = 1.0 - smoothstep(0.7, 1.0, hi / screen_len);
            hit = true;
            break;
        }
        s_prev = s_cur;
        s_cur = s_cur + stride;
    }
{% endif %}

    var reflection = vec3<f32>(0.0, 0.0, 0.0);
    if (hit) {
        let hc = vec2<i32>(hit_uv * vec2<f32>(full_dims));
        var hit_color: vec3<f32>;
        {% if glossy %}
        // M2b glossy: approximate the GGX reflection cone by averaging a small
        // screen-space disk of the reflected color, radius ∝ `spread` (roughness).
        // spread 0 → single tap (perfect mirror); rougher → wider, softer blur.
        // A golden-angle spiral spreads the 8 taps evenly. (A future refinement is
        // a prefiltered color mip pyramid + stochastic sampling + temporal reuse.)
        let blur_radius = spread * f32(full_dims.y) * 0.045;
        if (blur_radius < 0.75) {
            hit_color = textureLoad(color_tex, hc, 0).rgb;
        } else {
            var acc = vec3<f32>(0.0);
            for (var s = 0; s < 8; s = s + 1) {
                let fs = f32(s);
                let ang = fs * 2.3999632; // golden angle (radians)
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
        hit_color = textureLoad(color_tex, hc, 0).rgb;
        {% endif %}
        // Schlick Fresnel with the material's specular F0 (vec3): dielectrics
        // (F0≈0.04) are weak at normal incidence and ramp to white at grazing;
        // metals (F0=base color) reflect strongly and tinted at all angles.
        let f0 = reflectivity;
        let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - max(dot(n, v), 0.0), 5.0);
        // Fade toward the screen borders of the hit to hide the SS seam.
        let edge = min(min(hit_uv.x, 1.0 - hit_uv.x), min(hit_uv.y, 1.0 - hit_uv.y));
        let fade = smoothstep(0.0, max(params.edge_fade, 1e-4), edge);
        reflection = hit_color * fresnel * fade * travel_fade * params.intensity;
    }

    {% if half_res %}
    // Half-res trace: the guided upsample runs in the composite step.
    {% endif %}

    // Alpha = COVERAGE for real: 1 on a hit, 0 on a miss (sky/opt-out wrote 0
    // and returned above). The additive composite blends rgb, so a miss is a
    // no-op either way, but downstream consumers (the joint-bilateral upsample,
    // any future coverage-aware denoise) can trust the channel.
    let coverage = select(0.0, 1.0, hit);

    // Reflection-ONLY, premultiplied. Full-res invariant: composite_old +
    // reflection == the old base + reflection overwrite, since composite_old
    // == base at this pixel.
    textureStore(out_tex, coords, vec4<f32>(reflection, coverage));
}
