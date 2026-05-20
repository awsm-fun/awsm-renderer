// Shadow bind-group declarations. The bind-group slot is supplied by
// the containing template via `shadow_group_index` — opaque uses slot
// 3. Transparent is deferred to Phase 9.
//
// Bindings 0..=7 must stay in lockstep with
// `shared::material::bind_group::shadow_bind_group_layout_entries`.

const MAX_SHADOW_DESCRIPTORS: u32 = 32u;

struct ShadowDescriptor {
    // Light-space view-projection used at sample time.
    view_projection: mat4x4<f32>,
    // (atlas.x, atlas.y, atlas.w, atlas.h) in normalised UV space.
    atlas_rect: vec4<f32>,
    // (depth_bias, normal_bias, hardness, pcss_penumbra_scale)
    bias_params: vec4<f32>,
    // (split_far_view_z, cascade_index, cascade_count_in_light, evsm_flag)
    // `evsm_flag` is 1.0 when this cascade should sample EVSM moments
    // from `evsm_atlas` instead of the PCF depth atlas. Phase 5 lands
    // the flag + sample-site dispatch; the moment-write compute pass
    // and Gaussian blur are deferred — until they land, EVSM cascades
    // transparently fall back to PCF sampling on `shadow_atlas`.
    cascade_info: vec4<f32>,
};

struct ShadowGlobals {
    // (atlas.w, atlas.h, evsm.w, evsm.h)
    atlas_sizes: vec4<f32>,
    // (evsm_exponent, evsm_blur_radius, sscs_step_count, sscs_enabled)
    evsm_sscs: vec4<f32>,
    // (debug_cascade_colors, max_point_shadows, pad, pad)
    flags: vec4<u32>,
    // (cascade_array.w, cascade_array.h, max_layers, _) — per-layer
    // dimensions of the directional cascade texture array.
    cascade_array: vec4<f32>,
};

struct ShadowDescriptorArray {
    items: array<ShadowDescriptor, MAX_SHADOW_DESCRIPTORS>,
};

@group({{ shadow_group_index }}) @binding(0) var shadow_atlas: texture_depth_2d;
@group({{ shadow_group_index }}) @binding(1) var shadow_atlas_sampler: sampler_comparison;
@group({{ shadow_group_index }}) @binding(2) var shadow_cube_array: texture_depth_cube_array;
@group({{ shadow_group_index }}) @binding(3) var shadow_cube_sampler: sampler_comparison;
@group({{ shadow_group_index }}) @binding(4) var evsm_atlas: texture_2d<f32>;
@group({{ shadow_group_index }}) @binding(5) var evsm_atlas_sampler: sampler;
@group({{ shadow_group_index }}) @binding(6) var<uniform> shadow_globals: ShadowGlobals;
@group({{ shadow_group_index }}) @binding(7) var<uniform> shadow_descriptors: ShadowDescriptorArray;
@group({{ shadow_group_index }}) @binding(8) var shadow_cascade_array: texture_depth_2d_array;
@group({{ shadow_group_index }}) @binding(9) var shadow_cube_2d_array: texture_depth_2d_array;

// Sentinel for "no shadow" — packed into `LightPacked.row4.z`.
const SHADOW_INDEX_NONE: u32 = 0xFFFFFFFFu;

// 16 Poisson-distributed samples in `[-1, 1]^2`. Used by both the
// PCSS blocker search and the variable-kernel PCF pass. The same
// table doubled-up keeps the WGSL small; a per-pixel rotation breaks
// up the regular pattern.
const POISSON_DISK_16: array<vec2<f32>, 16> = array<vec2<f32>, 16>(
    vec2<f32>(-0.94201624, -0.39906216),
    vec2<f32>( 0.94558609, -0.76890725),
    vec2<f32>(-0.09418410, -0.92938870),
    vec2<f32>( 0.34495938,  0.29387760),
    vec2<f32>(-0.91588581,  0.45771432),
    vec2<f32>(-0.81544232, -0.87912464),
    vec2<f32>(-0.38277543,  0.27676845),
    vec2<f32>( 0.97484398,  0.75648379),
    vec2<f32>( 0.44323325, -0.97511554),
    vec2<f32>( 0.53742981, -0.47373420),
    vec2<f32>(-0.26496911, -0.41893023),
    vec2<f32>( 0.79197514,  0.19090188),
    vec2<f32>(-0.24188840,  0.99706507),
    vec2<f32>(-0.81409955,  0.91437590),
    vec2<f32>( 0.19984126,  0.78641367),
    vec2<f32>( 0.14383161, -0.14100790),
);

// Inter-leaved Gradient Noise — Jorge Jimenez's hash, returns a
// per-pixel angle in `[0, 2π]`. Used to rotate the Poisson disk so
// adjacent fragments don't sample identical patterns.
fn pcss_disk_angle(coords: vec2<f32>) -> f32 {
    let magic = vec3<f32>(0.06711056, 0.00583715, 52.9829189);
    let noise = fract(magic.z * fract(dot(coords, magic.xy)));
    return noise * 6.2831853;
}

fn pcss_rotate(v: vec2<f32>, sin_a: f32, cos_a: f32) -> vec2<f32> {
    return vec2<f32>(v.x * cos_a - v.y * sin_a, v.x * sin_a + v.y * cos_a);
}

// Screen-space contact shadows (SSCS). Short ray-march in view space
// from `world_pos` toward `light_dir` (the surface→light direction),
// using the already-bound depth buffer (`depth_tex`). Returns `[0, 1]`
// visibility — multiplied into the main shadow term to darken micro-
// occluders that the shadow map misses (gaps under feet, hair, etc.).
//
// `shadow_globals.evsm_sscs.w` is the master enable; `.z` is the step
// count. Phase 10 ships single-sample depth reads even when the
// geometry pass was rendered with MSAA (we read sample 0).
//
// The transparent pass doesn't bind a `depth_tex` (sampling the
// in-progress depth target on the same pass would be a feedback loop),
// so its shader template sets `sscs_available = false` and this
// function short-circuits to "fully lit" before any depth fetch.
fn apply_sscs(world_pos: vec3<f32>, light_dir: vec3<f32>) -> f32 {
{% if sscs_available %}
    let enabled = shadow_globals.evsm_sscs.w;
    if enabled < 0.5 {
        return 1.0;
    }
    let steps = u32(max(shadow_globals.evsm_sscs.z, 1.0));
    if steps == 0u {
        return 1.0;
    }

    // SSCS — Screen-Space Contact Shadows. A short ray-march from
    // each receiver toward the light, sampling the geometry-pass
    // depth buffer at each step. Used purely as a *contact-shadow
    // refinement* on top of the cascade map: it darkens the narrow
    // band right where caster geometry meets receiver geometry,
    // where the cascade's texel resolution leaves a "Peter Pan"
    // gap. It is NOT a substitute for the main shadow.
    //
    // The comparison is done in **linear view-space Z** (metres),
    // not NDC.z. This matters: NDC.z under perspective compresses
    // wildly with distance — a `0.001` NDC.z window covers ~1 mm at
    // the near plane but ~5 m at view-z = -50 m, so any NDC.z-based
    // thickness window misclassifies far receivers' rays against
    // unrelated background geometry. Earlier revisions had exactly
    // this failure mode (visible trails at zoom-out).
    //
    // Math:
    //   * receiver view-Z is `(camera.view · world_pos).z` (linear).
    //   * walking the ray `t_world` metres along `light_dir` changes
    //     view-Z by `(camera.view · light_dir).z · t_world` — also
    //     linear, so each march step's view-Z is exact.
    //   * the sampled depth-buffer texel is converted back to
    //     view-Z via `inv_proj`, which handles both perspective and
    //     ortho cameras correctly.
    //   * a scene texel "in front of the ray" satisfies
    //     `scene_view_z - ray_view_z > 0` (closer to camera = less
    //     negative). The thickness window is in metres and
    //     consistent across all depths.

    // Tunables — all are physical (metres or per-frame budget).
    // World-space step length is fixed so the same surface point
    // samples the same world positions every frame; only the depth
    // buffer read at each step's screen projection varies. This
    // matches the original Drobot 2017 formulation and avoids the
    // temporal jitter that a pixel-driven march produces (the
    // pixel-per-world ratio changes as the camera zooms, so a
    // fixed-pixel march samples different world positions every
    // frame even for the same surface).
    let SSCS_STEP_WORLD: f32 = 0.04;          // 4 cm per step → 64 cm reach @ 16 steps
    let SSCS_THICKNESS: f32 = 0.05;           // 5 cm slab counts as occluder
    let SSCS_SELF_OCCLUSION_EPS: f32 = 0.002; // 2 mm self-occlusion guard
    let MAX_DARKENING: f32 = 0.35;            // SSCS is refinement, not shadow

    let viewport_size = camera_raw.viewport.zw;
    let depth_dim = vec2<i32>(viewport_size);

    // Linear view-space Z values are used for the depth comparison
    // (NDC.z is non-linear under perspective — a fixed NDC.z window
    // would over/under-cover the slab at different depths and was
    // the bug behind the original "trailing at zoom-out" artefact).
    let recv_view_z = (camera_raw.view * vec4<f32>(world_pos, 1.0)).z;
    // View-Z slope per world-space metre along the ray; `light_dir`
    // is a direction vector (w = 0).
    let view_z_per_world = (camera_raw.view * vec4<f32>(light_dir, 0.0)).z;

    // World-space-stable per-fragment jitter on the start offset to
    // dither step quantisation between neighbouring receivers without
    // introducing per-frame noise. Hashing on the pixel coordinate
    // would change every camera move (same surface → different
    // pixel) which manifests as visible flicker; world-space
    // hashing is camera-invariant.
    let jitter_seed = world_pos.xz * 137.0
        + vec2<f32>(world_pos.y * 31.0, world_pos.y * 17.0);
    let jitter = pcss_disk_angle(jitter_seed) * (1.0 / 6.2831853);
    let t_start_world = (1.0 + jitter) * SSCS_STEP_WORLD;

    var hits: f32 = 0.0;
    for (var i: u32 = 0u; i < steps; i = i + 1u) {
        let t_world = t_start_world + SSCS_STEP_WORLD * f32(i);

        // Same world point every frame — project it now to find the
        // depth-buffer texel to sample.
        let ray_world = world_pos + light_dir * t_world;
        let clip = camera_raw.view_proj * vec4<f32>(ray_world, 1.0);
        if clip.w <= 0.0 {
            continue;
        }
        let ndc = clip.xyz / clip.w;
        if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 {
            continue;
        }
        let px_uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
        let px_f = px_uv * viewport_size;
        let px = vec2<i32>(px_f);
        if px.x < 0 || px.y < 0 || px.x >= depth_dim.x || px.y >= depth_dim.y {
            continue;
        }
        let scene_ndc_z = textureLoad(depth_tex, px, 0);
        if scene_ndc_z >= 1.0 {
            // Background — no occluder to find here.
            continue;
        }

        // Ray view-Z is linear in `t_world` — exact, no projection
        // round-trip needed.
        let ray_view_z = recv_view_z + view_z_per_world * t_world;
        // Linearise the sampled depth via the camera's inv_proj.
        // For perspective this is non-affine; for ortho it's a
        // simple scale. Either way the .z / .w form is correct.
        let scene_view_h = camera_raw.inv_proj
            * vec4<f32>(ndc.xy, scene_ndc_z, 1.0);
        let scene_view_z = scene_view_h.z / scene_view_h.w;

        // Both view-Z values are linear and negative for points in
        // front of the camera. A scene texel closer to the camera
        // than the ray has `scene_view_z > ray_view_z` (less
        // negative). The thickness slab keeps far-background
        // geometry from counting as an occluder.
        let dz = scene_view_z - ray_view_z;
        if dz > SSCS_SELF_OCCLUSION_EPS && dz < SSCS_THICKNESS {
            hits = hits + 1.0;
        }
    }

    let occluded = hits / f32(steps);
    return 1.0 - occluded * MAX_DARKENING;
{% else %}
    return 1.0;
{% endif %}
}

// Cube near plane — MUST match the value used in `Mat4::perspective_rh`
// for cube face generation in `Shadows::write_gpu`.
const POINT_SHADOW_NEAR: f32 = 0.05;

// Point-light cube shadow sample.
//
// Each cube face stores perspective NDC.z written by the rasterizer
// (90° FOV, `near = POINT_SHADOW_NEAR`, `far = light_range`). The
// projection is post-multiplied by a Y-flip on the writer side so the
// rasterized image lines up with WebGPU's D3D-style cube sampling
// convention (texel `t=0` → world +Y on the +X face, etc.) — see
// `Shadows::write_gpu`. That flip doesn't change NDC.z, so the depth
// formula below stays the same on both sides.
//
// The receiver recreates that NDC.z by projecting `length(light, P)`
// onto the *dominant* cube axis of the light-to-surface direction:
//
//     view_depth = distance(light, P) · |dir.major|
//     ndc_z      = (far / (far - near)) · (1 - near / view_depth)
//
// Same formula generates both the rasterized atlas value and the
// receiver reference, so they compare directly — no linear-depth FS
// override, no per-tap face recompute, no seam math.
fn sample_shadow_cube(desc: ShadowDescriptor, world_pos: vec3<f32>, world_normal: vec3<f32>) -> f32 {
    let light_pos = desc.atlas_rect.xyz;
    let range = max(desc.atlas_rect.w, 0.01);
    let slot = i32(desc.cascade_info.y);

    let biased_pos = world_pos + world_normal * desc.bias_params.y;
    let light_to_surface = biased_pos - light_pos;
    let dist = length(light_to_surface);
    if dist >= range {
        return 1.0;
    }
    let dir = light_to_surface / max(dist, 1e-4);

    // Major-axis (cube-face) projected depth.
    let abs_d = abs(dir);
    let major = max(abs_d.x, max(abs_d.y, abs_d.z));
    let view_depth = dist * max(major, 1e-4);

    // Same perspective NDC.z formula as the rasterizer.
    let near = POINT_SHADOW_NEAR;
    let ndc_z = (range / (range - near)) * (1.0 - near / max(view_depth, near));

    // Slope-aware constant bias. `n_dot_dir` floor at 0.05 keeps
    // grazing surfaces from running away to huge bias values
    // (`bias → ∞` as `n_dot_dir → 0`); the user-authored
    // `desc.bias_params.x` (the per-light `depth_bias`) is trusted
    // as-is. An earlier floor of `max(..., 0.001)` here silently
    // overrode any inspector value smaller than 0.001 — that was
    // ~10× the NDC gap between a receiver and a box's back face at
    // a typical 4 m point-light distance, so contacts could never
    // close even after lowering `depth_bias`. If you DO want a
    // global floor for some project, gate it on
    // `ShadowsConfig::min_point_depth_bias` (not present today).
    let n_dot_dir = abs(dot(dir, world_normal));
    let bias = desc.bias_params.x / max(n_dot_dir, 0.05);
    let ref_depth = clamp(ndc_z, 0.0, 1.0) - bias;
    let hardness = desc.bias_params.z;

    if hardness < 0.5 {
        return textureSampleCompareLevel(
            shadow_cube_array,
            shadow_cube_sampler,
            dir,
            slot,
            ref_depth,
        );
    }

    // Soft and PCSS share the same disc-on-tangent-plane tap layout:
    // each tap recomputes its own direction-from-light + NDC.z + bias
    // (rather than rotating the central `dir`) so a flat receiver
    // doesn't self-shadow into a kernel-shaped patch. The PCSS path
    // additionally does a blocker-search pre-pass using
    // `shadow_cube_2d_array` (raw depth reads) to scale the kernel.
    let abs_n = abs(world_normal);
    let up_hint = select(
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(1.0, 0.0, 0.0),
        abs_n.y > 0.99,
    );
    let tangent = normalize(cross(up_hint, world_normal));
    let bitangent = cross(world_normal, tangent);

    let angle = pcss_disk_angle(
        biased_pos.xz * 137.0 + vec2<f32>(biased_pos.y * 31.0, biased_pos.y * 17.0),
    );
    let sin_a = sin(angle);
    let cos_a = cos(angle);

    if hardness < 1.5 {
        // Soft — fixed 16-tap rotated Poisson, ~15 cm world disc.
        let SOFT_WORLD_RADIUS: f32 = 0.15;
        var sum = 0.0;
        for (var i = 0u; i < 16u; i = i + 1u) {
            let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * SOFT_WORLD_RADIUS;
            let tap_pos = biased_pos + tangent * off.x + bitangent * off.y;
            let tap_to_light = tap_pos - light_pos;
            let tap_dist = length(tap_to_light);
            let tap_dir = tap_to_light / max(tap_dist, 1e-4);
            let tap_abs = abs(tap_dir);
            let tap_major = max(tap_abs.x, max(tap_abs.y, tap_abs.z));
            let tap_view_depth = tap_dist * max(tap_major, 1e-4);
            let tap_ndc_z =
                (range / (range - near)) * (1.0 - near / max(tap_view_depth, near));
            let tap_n_dot_dir = abs(dot(tap_dir, world_normal));
            let tap_bias = desc.bias_params.x / max(tap_n_dot_dir, 0.05);
            let tap_ref = clamp(tap_ndc_z, 0.0, 1.0) - tap_bias;
            sum += textureSampleCompareLevel(
                shadow_cube_array,
                shadow_cube_sampler,
                tap_dir,
                slot,
                tap_ref,
            );
        }
        return sum / 16.0;
    }

    // PCSS — real blocker search + variable kernel.
    //
    // Stage 1 (blocker search): sample a fixed 16-tap "search" disc
    // sized by `pcss_penumbra_scale` (a virtual light disc radius in
    // metres). At each tap, project the tap's light direction onto
    // the right cube face, fetch raw depth via the 2D-array view,
    // and average the depths of taps that lie in front of the
    // receiver.
    //
    // Stage 2 (variable PCF): derive a penumbra radius from the
    // standard PCSS formula `(d_recv - d_avg) * light_size / d_avg`
    // and re-sample with `textureSampleCompareLevel`, this time
    // through the cube sampler so we get hardware bilinear PCF.
    //
    // The cube faces share a single NDC.z formula with the writer:
    //   ndc_z = (range / (range - near)) * (1 - near / view_depth)
    // so `textureLoad`-ed depths are directly comparable to the
    // per-tap `ref_depth` we compute here.
    let pcss_scale = max(desc.bias_params.w, 0.01);
    // Blocker-search disc: fixed 30 cm world radius scaled by
    // `pcss_penumbra_scale`. Bigger = fatter blocker estimate.
    let pcss_search_world_radius = 0.30 * pcss_scale;
    // Cube face dimension (px) for face-UV → texel conversion. All
    // faces share the same square resolution.
    let cube_dims = textureDimensions(shadow_cube_2d_array, 0);
    let cube_face_size = vec2<f32>(f32(cube_dims.x), f32(cube_dims.y));

    var blocker_sum = 0.0;
    var blocker_count = 0u;
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * pcss_search_world_radius;
        let tap_pos = biased_pos + tangent * off.x + bitangent * off.y;
        let tap_to_light = tap_pos - light_pos;
        let tap_dist = length(tap_to_light);
        let tap_dir = tap_to_light / max(tap_dist, 1e-4);
        let tap_abs = abs(tap_dir);
        let tap_major = max(tap_abs.x, max(tap_abs.y, tap_abs.z));
        let tap_view_depth = tap_dist * max(tap_major, 1e-4);
        let tap_ndc_z = clamp(
            (range / (range - near)) * (1.0 - near / max(tap_view_depth, near)),
            0.0,
            1.0,
        );
        // Inline cube-direction → (face, uv) projection. Standard
        // D3D cube convention; the writer's post-projection Y-flip is
        // already baked into the texel layout.
        var tap_face: u32 = 0u;
        var tap_uc: f32 = 0.0;
        var tap_vc: f32 = 0.0;
        var tap_ma: f32 = 1e-4;
        if tap_abs.x >= tap_abs.y && tap_abs.x >= tap_abs.z {
            if tap_dir.x > 0.0 {
                tap_face = 0u; tap_uc = -tap_dir.z; tap_vc = -tap_dir.y; tap_ma = tap_abs.x;
            } else {
                tap_face = 1u; tap_uc =  tap_dir.z; tap_vc = -tap_dir.y; tap_ma = tap_abs.x;
            }
        } else if tap_abs.y >= tap_abs.z {
            if tap_dir.y > 0.0 {
                tap_face = 2u; tap_uc =  tap_dir.x; tap_vc =  tap_dir.z; tap_ma = tap_abs.y;
            } else {
                tap_face = 3u; tap_uc =  tap_dir.x; tap_vc = -tap_dir.z; tap_ma = tap_abs.y;
            }
        } else {
            if tap_dir.z > 0.0 {
                tap_face = 4u; tap_uc =  tap_dir.x; tap_vc = -tap_dir.y; tap_ma = tap_abs.z;
            } else {
                tap_face = 5u; tap_uc = -tap_dir.x; tap_vc = -tap_dir.y; tap_ma = tap_abs.z;
            }
        }
        let tap_inv = 0.5 / max(tap_ma, 1e-4);
        let face_uv = vec2<f32>(tap_uc * tap_inv + 0.5, tap_vc * tap_inv + 0.5);
        let layer = i32(slot) * 6 + i32(tap_face);
        let tex_xy = clamp(
            vec2<i32>(face_uv * cube_face_size),
            vec2<i32>(0, 0),
            vec2<i32>(cube_dims.xy) - vec2<i32>(1, 1),
        );
        let d = textureLoad(shadow_cube_2d_array, tex_xy, layer, 0);
        // Bias-free blocker test — we want a clean estimate of how
        // many genuine occluders sit in front of the receiver. The
        // 0.0005 epsilon matches the directional PCSS path.
        if d < tap_ndc_z - 0.0005 {
            blocker_sum = blocker_sum + d;
            blocker_count = blocker_count + 1u;
        }
    }
    if blocker_count == 0u {
        return 1.0;
    }
    if blocker_count == 16u {
        return 0.0;
    }
    let avg_blocker = blocker_sum / f32(blocker_count);
    // PCSS penumbra in NDC.z space: `(z_recv - z_blocker) * light /
    // z_blocker`. Map back to a world-space disc radius on the
    // receiver tangent plane by treating the receiver-to-light
    // distance as the projection distance — light_size in world
    // metres = `pcss_penumbra_scale × 1m × penumbra_ratio`.
    let recv_ndc_z = clamp(ndc_z, 0.0, 1.0);
    let penumbra_ratio = clamp(
        (recv_ndc_z - avg_blocker) / max(avg_blocker, 1e-4),
        0.0,
        4.0,
    );
    // Clamp to keep the kernel between "more than Soft" (10 cm) and
    // "still affordable" (1 m world disc — already huge at typical
    // point-light scales).
    let penumbra_world_radius = clamp(
        pcss_search_world_radius * penumbra_ratio,
        0.10,
        1.00,
    );

    var sum = 0.0;
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * penumbra_world_radius;
        let tap_pos = biased_pos + tangent * off.x + bitangent * off.y;
        let tap_to_light = tap_pos - light_pos;
        let tap_dist = length(tap_to_light);
        let tap_dir = tap_to_light / max(tap_dist, 1e-4);
        let tap_abs = abs(tap_dir);
        let tap_major = max(tap_abs.x, max(tap_abs.y, tap_abs.z));
        let tap_view_depth = tap_dist * max(tap_major, 1e-4);
        let tap_ndc_z =
            (range / (range - near)) * (1.0 - near / max(tap_view_depth, near));
        let tap_n_dot_dir = abs(dot(tap_dir, world_normal));
        let tap_bias = desc.bias_params.x / max(tap_n_dot_dir, 0.05);
        let tap_ref = clamp(tap_ndc_z, 0.0, 1.0) - tap_bias;
        sum += textureSampleCompareLevel(
            shadow_cube_array,
            shadow_cube_sampler,
            tap_dir,
            slot,
            tap_ref,
        );
    }
    return sum / 16.0;
}


// EVSM sample. Reads the four exponential moments from `evsm_atlas`
// (written + blurred by the compute passes in `shadows::evsm`),
// reconstructs positive and negative one-tailed Chebyshev visibility,
// and returns `min(pos, neg)`. The pre-write blur is the source of
// softness — at sample time we do a single bilinear fetch.
//
// The exponent used at write time is `shadow_globals.evsm_sscs.x`
// (config.evsm_exponent). Receiver and writer must agree, else the
// curve mismatches and shadows go solid / clear.
fn chebyshev_upper(moments_2: vec2<f32>, t: f32) -> f32 {
    // moments_2.x = E[exp_z], moments_2.y = E[exp_z²].
    // variance = E[X²] − (E[X])²; clamped above a small floor so a
    // flat receiver doesn't divide by zero.
    let mean = moments_2.x;
    let variance = max(moments_2.y - mean * mean, 1e-5);
    let d = t - mean;
    if d <= 0.0 {
        return 1.0;
    }
    let p_max = variance / (variance + d * d);
    // Linstep light-bleed reduction — clamp the lower tail so partial
    // occluders don't lift the shadow into halftone.
    return clamp((p_max - 0.2) / 0.8, 0.0, 1.0);
}

fn sample_shadow_evsm(
    desc: ShadowDescriptor,
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
) -> f32 {
    let biased_pos = world_pos + world_normal * desc.bias_params.y;
    let clip = desc.view_projection * vec4<f32>(biased_pos, 1.0);
    if clip.w <= 0.0 {
        return 1.0;
    }
    let ndc = clip.xyz / clip.w;
    if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let uv_local = vec2<f32>(ndc.x * 0.5 + 0.5, -ndc.y * 0.5 + 0.5);
    let atlas_uv = desc.atlas_rect.xy + uv_local * desc.atlas_rect.zw;

    // Clamp to the EVSM cascade's own tile inset by half a texel so the
    // bilinear fetch never crosses the rect boundary. Without this the
    // 2×2 bilinear tap at the tile edge reads from neighbouring rect
    // moments (or uninitialised RGBA16F memory if no other EVSM
    // cascade was packed there), producing a hard rectangular cliff
    // exactly at the cascade outline. Same defence as the PCF path
    // does via `tile_min` / `tile_max`.
    let inv_evsm_atlas = vec2<f32>(
        1.0 / shadow_globals.atlas_sizes.z,
        1.0 / shadow_globals.atlas_sizes.w,
    );
    let evsm_tile_min = desc.atlas_rect.xy + 0.5 * inv_evsm_atlas;
    let evsm_tile_max = desc.atlas_rect.xy + desc.atlas_rect.zw - 0.5 * inv_evsm_atlas;
    let clamped_uv = clamp(atlas_uv, evsm_tile_min, evsm_tile_max);

    let moments = textureSampleLevel(evsm_atlas, evsm_atlas_sampler, clamped_uv, 0.0);
    let exponent = shadow_globals.evsm_sscs.x;
    // Map receiver depth [0,1] to the same [-1,1] space the writer
    // used (see `shadows::evsm::MOMENT_WRITE_WGSL`).
    let z = 2.0 * ndc.z - 1.0;
    let pos_t = exp(exponent * z);
    let neg_t = -exp(-exponent * z);
    let v_pos = chebyshev_upper(moments.xy, pos_t);
    let v_neg = chebyshev_upper(moments.zw, neg_t);
    return min(v_pos, v_neg);
}

// Sample a directional-cascade descriptor (kind = 3) backed by the
// `shadow_cascade_array` texture. Layout in atlas_rect:
//   .x = layer index (as f32)
//   .y = 0 (cascade starts at layer origin)
//   .zw = used sub-rect width/height in normalised UV
//
// Hardness branches mirror `sample_shadow_descriptor`'s 2D path; the
// only difference is the bound texture and an explicit layer argument
// on every compare/load.
fn sample_shadow_cascade_array(
    desc: ShadowDescriptor,
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
) -> f32 {
    let layer = i32(desc.atlas_rect.x);
    let biased_pos = world_pos + world_normal * desc.bias_params.y;
    let clip = desc.view_projection * vec4<f32>(biased_pos, 1.0);
    if clip.w <= 0.0 {
        return 1.0;
    }
    let ndc = clip.xyz / clip.w;
    if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let uv_local = vec2<f32>(ndc.x * 0.5 + 0.5, -ndc.y * 0.5 + 0.5);
    // Cascades always start at the layer origin; multiply by the
    // sub-rect size in normalised UV so smaller cascades don't read
    // outside their valid region.
    let atlas_uv = uv_local * desc.atlas_rect.zw;
    let ref_depth = ndc.z - desc.bias_params.x;
    let hardness = desc.bias_params.z;

    let inv_atlas = vec2<f32>(
        1.0 / shadow_globals.cascade_array.x,
        1.0 / shadow_globals.cascade_array.y,
    );
    // Half-texel inset to keep the bilinear / PCF taps inside the
    // valid sub-rect of the layer when `used_res < layer_size`.
    let tile_min = 0.5 * inv_atlas;
    let tile_max = desc.atlas_rect.zw - 0.5 * inv_atlas;

    if hardness < 0.5 {
        return textureSampleCompareLevel(
            shadow_cascade_array,
            shadow_atlas_sampler,
            clamp(atlas_uv, tile_min, tile_max),
            layer,
            ref_depth,
        );
    }
    if hardness < 1.5 {
        let world_per_texel = max(desc.cascade_info.y, 1e-4);
        let soft_world_radius = 0.25;
        let radius_texels = clamp(soft_world_radius / world_per_texel, 3.0, 20.0);
        let angle = pcss_disk_angle(
            biased_pos.xz * 137.0 + vec2<f32>(biased_pos.y * 31.0, biased_pos.y * 17.0),
        );
        let sin_a = sin(angle);
        let cos_a = cos(angle);
        var sum = 0.0;
        for (var i = 0u; i < 16u; i = i + 1u) {
            let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * radius_texels;
            sum += textureSampleCompareLevel(
                shadow_cascade_array, shadow_atlas_sampler,
                clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
                layer,
                ref_depth,
            );
        }
        return sum / 16.0;
    }
    // PCSS — same recipe as the 2D path, with the cascade-array
    // texture and explicit `layer` arg.
    let pcss_scale = max(desc.bias_params.w, 0.01);
    let world_per_texel_pcss = max(desc.cascade_info.y, 1e-4);
    let pcss_light_world_radius = 1.0 * pcss_scale;
    let atlas_uv_to_texels = vec2<f32>(
        shadow_globals.cascade_array.x,
        shadow_globals.cascade_array.y,
    );
    let angle = pcss_disk_angle(
        biased_pos.xz * 137.0 + vec2<f32>(biased_pos.y * 31.0, biased_pos.y * 17.0),
    );
    let sin_a = sin(angle);
    let cos_a = cos(angle);
    let search_radius_texels = clamp(
        pcss_light_world_radius / world_per_texel_pcss,
        4.0,
        64.0,
    );
    var blocker_sum = 0.0;
    var blocker_count = 0u;
    let tile_min_px = vec2<i32>(tile_min * atlas_uv_to_texels);
    let tile_max_px = vec2<i32>(tile_max * atlas_uv_to_texels);
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * search_radius_texels;
        let sample_uv = atlas_uv + off * inv_atlas;
        let coord = vec2<i32>(sample_uv * atlas_uv_to_texels);
        let c = clamp(coord, tile_min_px, tile_max_px);
        let d = textureLoad(shadow_cascade_array, c, layer, 0);
        if d < ref_depth - 0.0005 {
            blocker_sum = blocker_sum + d;
            blocker_count = blocker_count + 1u;
        }
    }
    if blocker_count == 0u {
        return 1.0;
    }
    if blocker_count == 16u {
        return 0.0;
    }
    let avg_blocker = blocker_sum / f32(blocker_count);
    let light_size_texels = pcss_light_world_radius / world_per_texel_pcss;
    let penumbra_texels = clamp(
        (ref_depth - avg_blocker) * light_size_texels / max(avg_blocker, 1e-4),
        4.0,
        40.0,
    );
    var pcf_sum = 0.0;
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * penumbra_texels;
        pcf_sum = pcf_sum + textureSampleCompareLevel(
            shadow_cascade_array,
            shadow_atlas_sampler,
            clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
            layer,
            ref_depth,
        );
    }
    return pcf_sum / 16.0;
}

// Sample a single shadow descriptor (cascade / spot / face). Returns
// `[0, 1]` visibility (1.0 = lit, 0.0 = fully shadowed).
//
// Hardness branches:
//   0.0 = Hard, 1-tap.
//   1.0 = Soft, 3x3 PCF.
//   2.0 = PCSS (phase 6 — currently falls back to Soft).
fn sample_shadow_descriptor(
    descriptor_index: u32,
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
) -> f32 {
    if descriptor_index >= MAX_SHADOW_DESCRIPTORS {
        return 1.0;
    }
    let desc = shadow_descriptors.items[descriptor_index];
    // cascade_info.w encodes the descriptor kind:
    //   0.0 = 2D PCF on `shadow_atlas` (spot)
    //   1.0 = 2D EVSM cascade — read moments from `evsm_atlas`
    //   2.0 = cube (point light)
    //   3.0 = directional cascade on `shadow_cascade_array`
    let kind = desc.cascade_info.w;
    if kind > 2.5 {
        return sample_shadow_cascade_array(desc, world_pos, world_normal);
    }
    if kind > 1.5 {
        return sample_shadow_cube(desc, world_pos, world_normal);
    }
    if kind > 0.5 {
        return sample_shadow_evsm(desc, world_pos, world_normal);
    }

    // Offset the receiver along its surface normal by `normal_bias`
    // world-space units before projecting into shadow space. This
    // pushes the sample point *toward* the light, which is how we
    // dodge acne on slanted surfaces without relying solely on a
    // constant depth bias (cascade Z-ranges differ a lot, so a flat
    // depth bias is either too soft or too aggressive). The
    // pipeline's slope-scale bias and `bias_params.x` depth bias
    // handle the residual.
    let biased_pos = world_pos + world_normal * desc.bias_params.y;
    let clip = desc.view_projection * vec4<f32>(biased_pos, 1.0);
    if clip.w <= 0.0 {
        return 1.0;
    }
    let ndc = clip.xyz / clip.w;
    if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    let uv_local = vec2<f32>(ndc.x * 0.5 + 0.5, -ndc.y * 0.5 + 0.5);
    let atlas_uv = desc.atlas_rect.xy + uv_local * desc.atlas_rect.zw;
    let ref_depth = ndc.z - desc.bias_params.x;
    let hardness = desc.bias_params.z;

    // PCF / PCSS taps must stay inside this cascade's tile of the
    // atlas. The tile-pack allocator places cascades edge-to-edge,
    // so a kernel that crosses the boundary samples a totally
    // unrelated cascade's depth (or another light's spot tile) and
    // produces a fringe of bogus shadow at the tile seam. The inset
    // is half a texel so bilinear PCF taps don't read past the edge
    // either.
    let inv_atlas = vec2<f32>(
        1.0 / shadow_globals.atlas_sizes.x,
        1.0 / shadow_globals.atlas_sizes.y,
    );
    let tile_min = desc.atlas_rect.xy + 0.5 * inv_atlas;
    let tile_max = desc.atlas_rect.xy + desc.atlas_rect.zw - 0.5 * inv_atlas;

    if hardness < 0.5 {
        return textureSampleCompareLevel(
            shadow_atlas,
            shadow_atlas_sampler,
            clamp(atlas_uv, tile_min, tile_max),
            ref_depth,
        );
    }
    if hardness < 1.5 {
        // Tap-rotated 16-sample Poisson disk PCF. The kernel is
        // sized in *world units* (`SOFT_WORLD_RADIUS`) and the
        // per-cascade texel-radius is recovered by dividing by the
        // cascade's `world_per_texel` (stored in `cascade_info.y`).
        // That keeps the perceived soft-edge width identical in every
        // cascade — without this, the near cascade's 2048 texels
        // covering a tiny world span produces razor-sharp shadows
        // while the far cascade's same 2048 texels covering a much
        // larger span produces soft ones, and the boundary between
        // is visible as a step in penumbra width.
        let world_per_texel = max(desc.cascade_info.y, 1e-4);
        // 25 cm penumbra at default light angles. Earlier passes used
        // 6 cm which produced shadows that were technically PCF but
        // visually indistinguishable from `Hard` — bumped to a value
        // that gives a clearly readable soft edge while still
        // resolving fine detail in the near cascade.
        let soft_world_radius = 0.25;
        // Clamp at 3 texels min (a too-tight kernel collapses to a
        // single 2×2 bilinear compare and the cascade-boundary blend
        // shows a "soft → razor" step). 20 texels max so the near
        // cascade doesn't waste kernel area where world_per_texel is
        // sub-millimetre.
        let radius_texels = clamp(soft_world_radius / world_per_texel, 3.0, 20.0);

        // Per-fragment rotation hash. MUST be keyed on world position
        // (not `atlas_uv`) — atlas_uv shifts by exactly one texel
        // every time the stable-fit's texel-snap moves, and the snap
        // moves whenever the camera translates by enough to cross a
        // texel boundary in light view. A pixel-keyed hash would
        // therefore rotate the tap pattern for every receiver in
        // lockstep on every such snap, producing a frame of
        // shimmer at every snap step. World-space hashing is
        // invariant under the camera's discrete grid jumps.
        let angle = pcss_disk_angle(
            biased_pos.xz * 137.0 + vec2<f32>(biased_pos.y * 31.0, biased_pos.y * 17.0),
        );
        let sin_a = sin(angle);
        let cos_a = cos(angle);
        var sum = 0.0;
        for (var i = 0u; i < 16u; i = i + 1u) {
            let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * radius_texels;
            sum += textureSampleCompareLevel(
                shadow_atlas, shadow_atlas_sampler,
                clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
                ref_depth,
            );
        }
        return sum / 16.0;
    }
    // PCSS — blocker-search + variable-kernel PCF.
    //
    // `pcss_penumbra_scale` (`bias_params.w`) is a multiplier on a
    // base 1 m "light disc" radius — i.e. how large the simulated
    // sun / area light appears at the receiver. With the default
    // scale = 1.0, the search & penumbra grow as if the light were a
    // 1 m disc; smaller values give sharper contact, larger values
    // give more dramatic falloff.
    //
    // Everything below is sized in *world units* (then converted to
    // texels via `world_per_texel` per cascade) so the cost / quality
    // of PCSS stays comparable across cascades — without that scaling
    // the search radius collapses to a few texels on the far cascade
    // and the algorithm degenerates into PCF.
    let pcss_scale = max(desc.bias_params.w, 0.01);
    let world_per_texel_pcss = max(desc.cascade_info.y, 1e-4);
    let pcss_light_world_radius = 1.0 * pcss_scale; // virtual light disc radius (m)
    let atlas_uv_to_texels = vec2<f32>(
        shadow_globals.atlas_sizes.x,
        shadow_globals.atlas_sizes.y,
    );
    // World-space rotation hash (see Soft PCF branch above — atlas
    // coordinates shift discretely with the stable-fit snap as the
    // camera moves, which would cause a frame of shimmer at every
    // texel jump; hashing on world position is invariant).
    let angle = pcss_disk_angle(
        biased_pos.xz * 137.0 + vec2<f32>(biased_pos.y * 31.0, biased_pos.y * 17.0),
    );
    let sin_a = sin(angle);
    let cos_a = cos(angle);
    // Blocker-search radius: track the light disc directly so a wider
    // virtual light sees more potential blockers (correct PCSS
    // behaviour — small light = sharper shadow because fewer
    // occluders matter). Bounded so it never collapses to under
    // 4 texels (a 4-texel search misses isolated blockers on near
    // cascades) and never exceeds a quarter of the tile (anything
    // larger reads almost the entire tile every sample).
    let search_radius_texels = clamp(
        pcss_light_world_radius / world_per_texel_pcss,
        4.0,
        64.0,
    );

    var blocker_sum = 0.0;
    var blocker_count = 0u;
    let tile_min_px = vec2<i32>(tile_min * atlas_uv_to_texels);
    let tile_max_px = vec2<i32>(tile_max * atlas_uv_to_texels);
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * search_radius_texels;
        let sample_uv = atlas_uv + off * inv_atlas;
        let coord = vec2<i32>(sample_uv * atlas_uv_to_texels);
        // Clamp to the cascade's own tile so the blocker search
        // doesn't read from an adjacent cascade's depth values.
        let c = clamp(coord, tile_min_px, tile_max_px);
        let d = textureLoad(shadow_atlas, c, 0);
        if d < ref_depth - 0.0005 {
            blocker_sum = blocker_sum + d;
            blocker_count = blocker_count + 1u;
        }
    }
    if blocker_count == 0u {
        return 1.0; // fully lit fast path
    }
    if blocker_count == 16u {
        // Every blocker-search sample was below the receiver's
        // biased depth — the receiver is deep inside the umbra
        // and the second 16-tap variable-kernel PCF would average
        // to ≈ 0 anyway. Skip it. Halves the work on fully-
        // shadowed receivers (the symmetric counterpart of the
        // fully-lit fast path above).
        return 0.0;
    }
    let avg_blocker = blocker_sum / f32(blocker_count);
    // Classic PCSS penumbra: `(d_receiver − d_blocker) · light_size /
    // d_blocker`, but with light_size expressed in *world units* via
    // `world_per_texel`. The clamps keep the kernel between "more
    // than `Soft`" (4 texels, so PCSS is always visibly softer than
    // `Soft`) and "still affordable" (40 texels — the 16-tap loop
    // already amortises hardware bilinear so this is fine on a
    // desktop GPU).
    let light_size_texels = pcss_light_world_radius / world_per_texel_pcss;
    let penumbra_texels = clamp(
        (ref_depth - avg_blocker) * light_size_texels / max(avg_blocker, 1e-4),
        4.0,
        40.0,
    );
    var pcf_sum = 0.0;
    for (var i = 0u; i < 16u; i = i + 1u) {
        let off = pcss_rotate(POISSON_DISK_16[i], sin_a, cos_a) * penumbra_texels;
        pcf_sum = pcf_sum + textureSampleCompareLevel(
            shadow_atlas,
            shadow_atlas_sampler,
            clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
            ref_depth,
        );
    }
    return pcf_sum / 16.0;
}

// Per-light cascade selection with smooth blending across split
// boundaries. `descriptor_base` points to the first cascade descriptor
// of a directional light; `cascade_info.z` gives the cascade count.
//
// We walk descriptors descriptor_base..base+count and pick the first
// whose `cascade_info.x` (split_far in world-space depth) exceeds
// `view_z`. To hide the abrupt softness jump that comes from each
// successive cascade halving its atlas resolution, the last
// `CASCADE_BLEND` fraction of every cascade's depth range linearly
// fades into the next cascade's sample (or to fully lit for the
// final cascade — receivers past the very end get no shadow).
//
// Returns 1.0 (no shadow) if `view_z` is beyond the last cascade.
// Fraction of each cascade's depth range that fades into the next
// cascade. Stretching this band wider spreads the (unavoidable)
// quality difference between cascades across a larger area, which
// the eye stops reading as a hard edge AND keeps receivers near
// cascade boundaries from flickering when the camera moves them
// across the boundary in discrete texel-snap jumps. 50% is the AAA
// default — the corresponding `BLEND_OVERLAP` in `fit_cascades`
// ensures the next cascade's frustum covers this whole band.
const CASCADE_BLEND: f32 = 0.5;

fn sample_shadow_directional(
    descriptor_base: u32,
    world_pos: vec3<f32>,
    world_normal: vec3<f32>,
    view_z: f32,
) -> f32 {
    if descriptor_base == SHADOW_INDEX_NONE {
        return 1.0;
    }
    if descriptor_base >= MAX_SHADOW_DESCRIPTORS {
        return 1.0;
    }
    // Point/cube lights: single descriptor, no cascade walk. Their
    // `view_projection` is intentionally `Mat4::ZERO` (the cube path
    // uses `atlas_rect.xyz/.w` = (light_pos, range) + a world-space
    // direction instead of a projection), so the cascade picker's
    // `cand_clip.w <= 0.0` test below would reject them and silently
    // return "fully lit". Dispatch straight to `sample_shadow_descriptor`
    // which routes cube descriptors to `sample_shadow_cube`.
    //
    // Kind values: 0.0 = 2D PCF (spot), 1.0 = 2D EVSM (cascade),
    // 2.0 = cube, 3.0 = cascade-array PCF (directional). Only kind=2.0
    // is the single-descriptor short-circuit; the cascade-array case
    // still needs the cascade walk because directional lights pack
    // multiple cascades.
    let base_kind = shadow_descriptors.items[descriptor_base].cascade_info.w;
    if base_kind > 1.5 && base_kind < 2.5 {
        return sample_shadow_descriptor(descriptor_base, world_pos, world_normal);
    }
    let cascade_count = u32(shadow_descriptors.items[descriptor_base].cascade_info.z);
    // Cascade pick: walk descriptors near→far and stop at the first
    // one that contains the receiver in *both* depth (`view_z` inside
    // the cascade's split range) AND lateral NDC (clip.xy ∈ [-1, 1]).
    //
    // The lateral check is what we used to silently miss — picking
    // purely by `view_z` then projecting could land us on a cascade
    // whose XY frustum clipped the receiver, and `sample_shadow_descriptor`
    // would short-circuit to "fully lit". That produced a hard
    // diagonal cliff at each cascade's lateral edge whenever the
    // outer cascade actually had coverage there. Falling through to
    // the next cascade outward keeps the shadow continuous across
    // lateral boundaries the same way the depth-axis blend handles
    // split boundaries.
    var picked: u32 = SHADOW_INDEX_NONE;
    var picked_local: u32 = 0u;
    for (var i = 0u; i < cascade_count; i = i + 1u) {
        let idx = descriptor_base + i;
        if idx >= MAX_SHADOW_DESCRIPTORS {
            break;
        }
        let split_far = shadow_descriptors.items[idx].cascade_info.x;
        if view_z > split_far {
            continue;
        }
        let cand = shadow_descriptors.items[idx];
        let cand_clip = cand.view_projection * vec4<f32>(world_pos, 1.0);
        if cand_clip.w <= 0.0 {
            continue;
        }
        let cand_ndc = cand_clip.xyz / cand_clip.w;
        if cand_ndc.x < -1.0 || cand_ndc.x > 1.0
            || cand_ndc.y < -1.0 || cand_ndc.y > 1.0
            || cand_ndc.z < 0.0 || cand_ndc.z > 1.0
        {
            continue;
        }
        picked = idx;
        picked_local = i;
        break;
    }
    if picked == SHADOW_INDEX_NONE {
        return 1.0;
    }
    let split_far = shadow_descriptors.items[picked].cascade_info.x;
    var split_near: f32 = 0.0;
    if picked_local > 0u {
        split_near = shadow_descriptors.items[picked - 1u].cascade_info.x;
    }
    let span = max(split_far - split_near, 1e-4);
    let normalized = clamp((view_z - split_near) / span, 0.0, 1.0);

    let primary = sample_shadow_descriptor(picked, world_pos, world_normal);
    if normalized < 1.0 - CASCADE_BLEND {
        return primary;
    }
    let blend_t = (normalized - (1.0 - CASCADE_BLEND)) / CASCADE_BLEND;
    let next_local = picked_local + 1u;
    if next_local >= cascade_count {
        // Final cascade fades to fully lit at the very edge of the
        // light's max_distance so receivers don't pop from shadowed
        // to lit.
        return mix(primary, 1.0, blend_t);
    }
    let next_idx = descriptor_base + next_local;
    if next_idx >= MAX_SHADOW_DESCRIPTORS {
        return primary;
    }
    let secondary = sample_shadow_descriptor(next_idx, world_pos, world_normal);
    return mix(primary, secondary, blend_t);
}

// DEBUG: returns the picked cascade index (0..3) as a float, or
// 4.0 if no cascade was picked. Mirrors `sample_shadow_directional`'s
// picker so the colour overlay matches what shadow sampling actually
// uses — both the `view_z` split test AND the lateral NDC test.
fn debug_picked_cascade(
    descriptor_base: u32,
    world_pos: vec3<f32>,
    view_z: f32,
) -> f32 {
    if descriptor_base == SHADOW_INDEX_NONE || descriptor_base >= MAX_SHADOW_DESCRIPTORS {
        return 4.0;
    }
    let cascade_count = u32(shadow_descriptors.items[descriptor_base].cascade_info.z);
    for (var i = 0u; i < cascade_count; i = i + 1u) {
        let idx = descriptor_base + i;
        if idx >= MAX_SHADOW_DESCRIPTORS {
            break;
        }
        let desc = shadow_descriptors.items[idx];
        if view_z > desc.cascade_info.x {
            continue;
        }
        let clip = desc.view_projection * vec4<f32>(world_pos, 1.0);
        if clip.w <= 0.0 {
            continue;
        }
        let ndc = clip.xyz / clip.w;
        if ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
            continue;
        }
        return f32(i);
    }
    return 4.0;
}

// Debug-overlay tint for cascade visualisation. Driven by
// `shadow_globals.flags.x` (`debug_cascade_colors`). Returns the
// cascade-tinted color if enabled, otherwise the input unchanged.
//
// The palette additionally distinguishes EVSM cascades from PCF
// cascades — EVSM cascades get a warm tone (orange / yellow) while
// PCF cascades get a cool tone (red / green / blue). The
// `cascade_info.w` flag is the source of truth (1.0 → EVSM, 0.0 →
// PCF), set on the writer side in `Shadows::write_gpu`.
fn debug_cascade_tint(
    base_color: vec3<f32>,
    descriptor_base: u32,
    world_pos: vec3<f32>,
    view_z: f32,
) -> vec3<f32> {
    if shadow_globals.flags.x == 0u {
        return base_color;
    }
    let picked = debug_picked_cascade(descriptor_base, world_pos, view_z);
    let picked_idx = u32(picked);
    if picked_idx >= 4u {
        return base_color;
    }
    // PCF (cool): red / green / blue / cyan
    let pcf_palette = array<vec3<f32>, 4>(
        vec3<f32>(1.0, 0.3, 0.3),
        vec3<f32>(0.3, 1.0, 0.3),
        vec3<f32>(0.3, 0.5, 1.0),
        vec3<f32>(0.3, 0.9, 1.0),
    );
    // EVSM (warm): scarlet / orange / yellow / gold. The receiver-
    // side dispatch uses `cascade_info.w > 0.5` for "this descriptor
    // is EVSM"; mirror that here so the overlay tracks reality.
    let evsm_palette = array<vec3<f32>, 4>(
        vec3<f32>(1.0, 0.4, 0.1),
        vec3<f32>(1.0, 0.6, 0.1),
        vec3<f32>(1.0, 0.85, 0.1),
        vec3<f32>(1.0, 1.0, 0.3),
    );
    let idx = descriptor_base + picked_idx;
    let kind = shadow_descriptors.items[idx].cascade_info.w;
    // EVSM (kind = 1.0) → warm palette; PCF flavours (cascade-array
    // kind = 3.0 and the 2D-atlas spot kind = 0.0) → cool palette.
    let is_evsm = kind > 0.5 && kind < 1.5;
    let tint = select(pcf_palette[picked_idx], evsm_palette[picked_idx], is_evsm);
    return mix(base_color, tint, 0.35);
}
