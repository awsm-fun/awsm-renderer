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
// The glossy / temporal / half_res template blocks are the structural
// permutation axes (§5a): each compiles ONLY into the variant that needs it, so
// Mirror carries none of the glossy/denoise code, non-temporal none of the
// reproject code, etc.

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
    frame: f32,     // monotonic; temporal variant rotates the march jitter by it
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
{% if temporal %}
// M3 temporal reprojection: previous-frame accumulated reflection (filtered
// via the linear sampler at the reprojected UV) + this-frame history write.
// These bindings + the linear sampler exist ONLY on the temporal variant — the
// non-temporal trace probes everything with integer textureLoad and binds none.
@group(0) @binding(7) var history_prev_tex: texture_2d<f32>;
@group(0) @binding(8) var history_sampler: sampler;
@group(0) @binding(9) var history_curr_tex: texture_storage_2d<rgba16float, write>;
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

    let steps = max(i32(params.max_steps), 1);
    // GEOMETRIC stride: near reflections (the visually dominant contacts —
    // floor reflections of nearby geometry) march at centimetre precision so
    // thin features (neon tubes, trims) are never skipped, while the stride
    // grows ~6%/step to still reach `max_distance` with the same step count.
    // A UNIFORM stride here was the "dashed reflection" artifact: at
    // max_distance 100+ / 96 steps every stride was ~1 m, larger than the
    // thin geometry being reflected, so hits landed intermittently.
    let growth = 1.06;
    let base_len = params.max_distance * (growth - 1.0) / (pow(growth, f32(steps)) - 1.0);

    // Interleaved-gradient-noise jitter of the FIRST step, per pixel:
    // decorrelates the stride phase between neighbouring pixels, turning
    // coherent staircase banding into unstructured (and far less visible)
    // noise. Deterministic per pixel — stable under a static camera.
    let ign = fract(52.9829189 * fract(dot(vec2<f32>(coords), vec2<f32>(0.06711056, 0.00583715))));
    {% if temporal %}
    // Rotate by the golden ratio each frame: the history blend then averages
    // the march phase over ~1/(1-temporal_weight) frames, converging the
    // stipple on bright reflections to a smooth result.
    let jitter = fract(ign + params.frame * 0.61803398875);
    {% else %}
    let jitter = ign;
    {% endif %}

    var hit = false;
    var hit_uv = vec2<f32>(0.0, 0.0);
    var step_len = base_len;
    var t_prev = 0.0;
    var t = base_len * (0.5 + jitter);

    // Linear DDA march in view space. depth_tex is non-filterable, so
    // every scene-depth probe is an integer textureLoad.
    for (var i = 0; i < steps; i = i + 1) {
        let pi = p + refl * t;
        let proj = view_to_uv(pi, cam);
        if (proj.z < 0.0 || proj.x < 0.0 || proj.x > 1.0 || proj.y < 0.0 || proj.y > 1.0) {
            break;
        }
        let scoords = vec2<i32>(proj.xy * vec2<f32>(full_dims));
        let sdepth = textureLoad(depth_tex, scoords, 0);
        let scene_p = view_pos_from_depth(proj.xy, sdepth, cam);
        // View looks down -Z: positive linear depth = -z.
        let ray_z = -pi.z;
        let scene_z = -scene_p.z;
        // Thickness scales with the local stride: a fixed world thickness
        // combined with growing steps either false-hits far away (too thick
        // for fine strides) or tunnels through surfaces near the march tail
        // (too thin for coarse strides). `params.thickness` acts as the floor.
        let thickness = max(params.thickness, step_len * 1.5);
        if (ray_z > scene_z && (ray_z - scene_z) < thickness) {
            // Binary refinement between the last miss and this hit.
            var lo = t_prev;
            var hi = t;
            for (var b = 0; b < 5; b = b + 1) {
                let mid = 0.5 * (lo + hi);
                let pm = p + refl * mid;
                let pj = view_to_uv(pm, cam);
                let mc = vec2<i32>(pj.xy * vec2<f32>(full_dims));
                let md = textureLoad(depth_tex, mc, 0);
                let mz = -view_pos_from_depth(pj.xy, md, cam).z;
                if (-pm.z > mz) { hi = mid; } else { lo = mid; }
            }
            let pf = p + refl * hi;
            hit_uv = view_to_uv(pf, cam).xy;
            hit = true;
            break;
        }
        t_prev = t;
        step_len = step_len * growth;
        t = t + step_len;
    }

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
        reflection = hit_color * fresnel * fade * params.intensity;
    }

    {% if temporal %}
    // M3 temporal reprojection — SINGLE-PASS depth reproject + on-screen reject.
    // Reconstruct this pixel's WORLD position from view-space `p`, project it
    // with the PREVIOUS frame's view-projection to find where the surface was
    // last frame, and (if that reprojected UV lands on-screen) blend the
    // filtered history reflection in by `params.temporal_weight`. The on-screen
    // test is the disocclusion reject: newly-revealed pixels reproject
    // off-screen and keep this frame's fresh trace.
    //
    // OUT OF SCOPE for M3: a full spatial neighbourhood-AABB colour clamp (which
    // suppresses ghosting on moving reflectors) needs a second resolve pass —
    // this is deliberately the single-pass depth-reproject + reject deliverable.
    let world_pos = (cam.inv_view * vec4<f32>(p, 1.0)).xyz;
    let prev_clip = cam.prev_view_proj * vec4<f32>(world_pos, 1.0);
    if (prev_clip.w > 0.0) {
        let prev_ndc = prev_clip.xyz / prev_clip.w;
        let prev_uv = vec2<f32>(prev_ndc.x * 0.5 + 0.5, 1.0 - (prev_ndc.y * 0.5 + 0.5));
        if (prev_uv.x >= 0.0 && prev_uv.x <= 1.0 && prev_uv.y >= 0.0 && prev_uv.y <= 1.0) {
            let hist = textureSampleLevel(history_prev_tex, history_sampler, prev_uv, 0.0);
            reflection = mix(reflection, hist.rgb, params.temporal_weight);
        }
    }
    {% endif %}
    {% if half_res %}
    // Half-res trace: the guided upsample runs in the composite step.
    {% endif %}

    // Alpha = COVERAGE for real: 1 on a hit, 0 on a miss (sky/opt-out wrote 0
    // and returned above). The additive composite blends rgb, so a miss is a
    // no-op either way, but downstream consumers (the joint-bilateral upsample,
    // any future coverage-aware denoise) can trust the channel.
    let coverage = select(0.0, 1.0, hit);

    {% if temporal %}
    // Persist the blended result so next frame's reprojection reads it.
    textureStore(history_curr_tex, coords, vec4<f32>(reflection, coverage));
    {% endif %}

    // Reflection-ONLY, premultiplied. Full-res invariant: composite_old +
    // reflection == the old base + reflection overwrite, since composite_old
    // == base at this pixel.
    textureStore(out_tex, coords, vec4<f32>(reflection, coverage));
}
