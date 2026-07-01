// Shadow bind-group declarations. The bind-group slot is supplied by
// the containing template via `shadow_group_index` — opaque uses slot
// 3. The transparent pass currently doesn't bind these (the adapter's
// `maxBindGroups=4` budget is fully consumed by transparent's existing
// groups).
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
    // from `evsm_atlas` instead of the PCF depth atlas. The flag +
    // sample-site dispatch are wired; the moment-write compute pass
    // and Gaussian blur landed alongside it. If a future tweak leaves
    // EVSM disabled the cascade falls back to PCF on `shadow_atlas`.
    cascade_info: vec4<f32>,
    // (shadow_samples, pad, pad, pad) — per-light soft/PCSS Vogel tap budget
    // (.x). Universal slot: kernel_slack overloads cascade_info.x and is only
    // free on the cube path, so the tap count (read by every light kind) needs
    // its own. Read via `shadow_tap_count(desc.extra_params.x)`.
    extra_params: vec4<f32>,
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

// Sentinel for "no shadow" — packed into `LightPacked.row4.z`. Kept ungated:
// `apply_lighting` compares against it even before any shadow sample, and an
// unused const is free.
const SHADOW_INDEX_NONE: u32 = 0xFFFFFFFFu;

// ── Shadow SAMPLING (PCSS / PCF / EVSM / cube + SSCS) ───────────────────────
// Called ONLY from `apply_lighting`, so the whole block is gated on
// `needs_shadow_sampling` (= inc.apply_lighting). Materials that don't run
// first-party lighting (every custom material + unlit/toon/flipbook + the empty
// kernel) drop this ~50 KB of WGSL entirely. The bind group + structs above stay
// (ABI — the pipeline layout always has the shadow group).
{% if needs_shadow_sampling %}

// Baked Vogel (sunflower) disc — the single sampling pattern for ALL shadow
// kinds (cube + cascade + spot; PCF, blocker search, and PCSS). Golden-angle
// spiral: even coverage with no clumps at any count, so a per-pixel phase
// rotation decorrelates neighbours WITHOUT exposing clumps as speckle (the
// failure mode of rotating a fixed Poisson set over a wide kernel).
//
// VOGEL_BASE[i] = sqrt(i+0.5) * (cos(i*GA), sin(i*GA)). The runtime point for
// an n-tap disc is `rotate(VOGEL_BASE[i], phase) * inversesqrt(n)`, which
// equals `sqrt((i+0.5)/n)` at angle `i*GA + phase`. Splitting the radius this
// way is the whole trick: the only n-dependent factor is `inversesqrt(n)`,
// computed ONCE per pixel — so the tap count `n` is a free runtime parameter
// (truncating to the first n entries × rsqrt(n) still fills the full unit disc)
// AND there are ZERO transcendentals per tap (vs sqrt+sin+cos in the naive
// formula). `n` may be any value up to VOGEL_MAX_TAPS.
const VOGEL_MAX_TAPS: u32 = 64u;
const VOGEL_BASE: array<vec2<f32>, 64> = array<vec2<f32>, 64>(
    vec2<f32>(0.707107, 0.000000),
    vec2<f32>(-0.903089, 0.827303),
    vec2<f32>(0.138232, -1.575085),
    vec2<f32>(1.138285, 1.484691),
    vec2<f32>(-2.088893, -0.369496),
    vec2<f32>(1.978782, -1.258739),
    vec2<f32>(-0.661864, 2.462100),
    vec2<f32>(-1.262246, -2.430378),
    vec2<f32>(2.738569, 1.000121),
    vec2<f32>(-2.849024, 1.176036),
    vec2<f32>(1.373418, -2.934914),
    vec2<f32>(1.014921, 3.235728),
    vec2<f32>(-3.058984, -1.772744),
    vec2<f32>(3.588536, -0.788930),
    vec2<f32>(-2.190028, 3.115089),
    vec2<f32>(-0.505947, -3.904359),
    vec2<f32>(3.106019, 2.617756),
    vec2<f32>(-4.179728, 0.172845),
    vec2<f32>(3.048791, -3.033954),
    vec2<f32>(-0.203976, 4.411167),
    vec2<f32>(-2.900934, -3.476288),
    vec2<f32>(4.595400, 0.618305),
    vec2<f32>(-3.893673, 2.709116),
    vec2<f32>(1.063975, -4.729477),
    vec2<f32>(2.460920, 4.294633),
    vec2<f32>(-4.810863, -1.534796),
    vec2<f32>(4.673141, -2.159110),
    vec2<f32>(-2.024521, 4.837490),
    vec2<f32>(-1.806841, -5.023478),
    vec2<f32>(4.807809, 2.526850),
    vec2<f32>(-5.340266, 1.407677),
    vec2<f32>(3.035444, -4.720813),
    vec2<f32>(0.965593, 5.618508),
    vec2<f32>(-4.576062, -3.543960),
    vec2<f32>(5.853615, -0.484960),
    vec2<f32>(-4.046082, 4.373696),
    vec2<f32>(0.029472, -6.041451),
    vec2<f32>(4.114440, 4.535569),
    vec2<f32>(-6.178359, -0.572609),
    vec2<f32>(5.006298, -3.799602),
    vec2<f32>(-1.139045, 6.261196),
    vec2<f32>(-3.431072, -5.452316),
    vec2<f32>(6.287361, 1.723105),
    vec2<f32>(-5.867884, 3.011301),
    vec2<f32>(2.318893, -6.254817),
    vec2<f32>(2.543292, 6.247533),
    vec2<f32>(-6.162111, -2.920340),
    vec2<f32>(6.586106, -2.030567),
    vec2<f32>(-3.521252, 6.008393),
    vec2<f32>(-1.477146, -6.878811),
    vec2<f32>(5.793419, 4.115373),
    vec2<f32>(-7.121259, 0.887509),
    vec2<f32>(4.696434, -5.517563),
    vec2<f32>(0.266559, 7.309511),
    vec2<f32>(-5.181816, -5.258211),
    vec2<f32>(7.440113, 0.380418),
    vec2<f32>(-5.794585, 4.787775),
    vec2<f32>(1.047803, -7.510134),
    vec2<f32>(4.337639, 6.299594),
    vec2<f32>(-7.517192, -1.729688),
    vec2<f32>(6.767494, -3.834192),
    vec2<f32>(-2.419935, 7.459485),
    vec2<f32>(-3.280777, -7.192809),
    vec2<f32>(7.335807, 3.112224),
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

// The i-th tap of an n-tap Vogel disc, in the unit disk. Caller hoists the two
// per-pixel scalars (`rsqrt_n = inversesqrt(f32(n))`, and `sin_p`/`cos_p` of
// the IGN phase) out of the loop, so this is just a rotate + scale per tap — no
// transcendentals. `i` must be < n <= VOGEL_MAX_TAPS.
fn vogel_tap(i: u32, rsqrt_n: f32, sin_p: f32, cos_p: f32) -> vec2<f32> {
    return pcss_rotate(VOGEL_BASE[i], sin_p, cos_p) * rsqrt_n;
}

// Per-light Vogel tap budget (PCF / soft / final-PCSS), from the descriptor's
// `extra_params.x`. Clamped to [VOGEL_MIN_TAPS, VOGEL_MAX_TAPS]; 0 (unset)
// falls back to VOGEL_DEFAULT_TAPS so an un-plumbed descriptor still samples.
//
// PERF NOTE — `n` is a DYNAMIC loop bound below, which usually rings alarm
// bells ("dynamic loop per texel = killer"). It isn't, here, for a specific
// reason: `n` comes from the per-LIGHT descriptor, so it's UNIFORM across the
// warp (every lane sampling a given light's shadow iterates the same count).
// The killer case is a per-PIXEL-varying trip count → lane divergence (the old
// `pcss_tap_count(ndc.z)` taper, since removed). A uniform dynamic bound has no
// divergence; the only cost vs a compile-time constant is lost loop unrolling /
// latency-hiding, which is small on these texture-fetch-bound loops (and may not
// even differ — drivers often don't fully unroll a 32-tap textureSampleCompare
// loop regardless). Unmeasured, deliberately: kept per-light because it's a
// strict superset (set every light the same → it behaves as one global knob,
// for free) at negligible cost.
//
// IF a profile ever shows this dynamic bound actually costing something: the
// clean fast-path is shader specialization via the askama template + a cache-key
// dimension — when Rust sees ALL active casters share a count N, key the
// material_prep variant on `Some(N)` and emit `const` tap counts (→ unrolled);
// key on `None` for the mixed case and fall back to exactly this dynamic path.
// All-or-nothing per pass, recompiles when the shared N changes (debounce the
// editor slider). Not built — premature without a measured delta.
const VOGEL_MIN_TAPS: u32 = 8u;
const VOGEL_DEFAULT_TAPS: u32 = 16u;
fn shadow_tap_count(extra_x: f32) -> u32 {
    let n = u32(extra_x + 0.5);
    if n == 0u {
        return VOGEL_DEFAULT_TAPS;
    }
    return clamp(n, VOGEL_MIN_TAPS, VOGEL_MAX_TAPS);
}

// Blocker-search budget: a fraction of the PCF budget (the search only
// estimates an average blocker depth, so it needs fewer taps; the denoise
// blur + the averaging smooth the residual width noise). Min 8.
fn shadow_blocker_count(n: u32) -> u32 {
    return max((n * 3u) / 4u, VOGEL_MIN_TAPS);
}


// Screen-space contact shadows (SSCS). Short ray-march in view space
// from `world_pos` toward `light_dir` (the surface→light direction),
// using the already-bound depth buffer (`depth_tex`). Returns `[0, 1]`
// visibility — multiplied into the main shadow term to darken micro-
// occluders that the shadow map misses (gaps under feet, hair, etc.).
//
// `shadow_globals.evsm_sscs.w` is the master enable; `.z` is the step
// count. Uses single-sample depth reads even when the geometry pass
// was rendered with MSAA (we read sample 0).
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

// Texel-quantization receiver-plane slack for the point-light soft/PCSS
// taps is the per-light `kernel_slack` shadow param, delivered in the
// otherwise-unused `cascade_info.x` descriptor slot (see
// `shadows::state` cube packing). A tap whose light-direction lands in a
// cube texel storing the floor's own back-face depth self-shadows by the
// depth quantization across that texel; the constant per-tap `depth_bias`
// can't cover it, so the average dips below 1.0 in radial bands → the
// "acne rings" on a flat floor under a point light.
//
// The slack widens the comparison bias by exactly that quantization gap:
// `tap_grad × world_per_texel × kernel_slack`, where `world_per_texel =
// 2·view_depth / cube_resolution` is the world footprint of ONE cube
// texel at the tap's distance. `kernel_slack` therefore reads as "how
// many texels of quantization to forgive" (default 2).
//
// CRITICAL: the slack scales with ONE TEXEL, not the kernel radius. The
// earlier formula used the kernel *radius* (`SOFT_WORLD_RADIUS` /
// `penumbra_world_radius`), so a ~1 m PCSS penumbra demanded ~100× the
// slack a genuine receiver↔occluder gap provides, and the umbra leaked
// to lit — worst directly under a floating occluder, where the gap is
// smallest. One-texel scale fixes the quantization acne identically
// (it IS a one-texel effect) while staying far too small to ever bridge
// a real occluder gap, so the umbra can't leak. Hard (no kernel) gets
// zero extra bias and stays crisp.
//
// The stored `kernel_slack` value (default 2) is reinterpreted by this
// change: under the old kernel-radius formula the same number meant a
// far larger world slack. No migration is provided — the kernel-radius
// formula only ever shipped in the immediately-prior point-light commit,
// so a project with a non-default value tuned against it is not expected
// in practice, and the default reads correctly here. If an old project
// shows faint acne rings, nudge its per-light `kernel_slack` up a texel.

// World-space → NDC.z depth gradient at a projected point, for ANY projection.
//
//   ndc.z = clip.z / clip.w,   clip = view_projection · world_pos
//   d(ndc.z)/d(world_pos) = (row2·clip.w − row3·clip.z) / clip.w²   (a vec3)
//
// where row2 / row3 are the z- and w-rows of `view_projection`. The returned
// scalar is the magnitude — how much ndc.z moves per world metre along the
// steepest (≈ light-ray) direction. Multiply a WORLD-space depth bias by it to
// get the NDC.z offset to subtract.
//
// Why this matters: a *constant* NDC.z bias (`ref_depth = ndc.z − depth_bias`)
// is only distance-invariant under an orthographic projection, where ndc.z is
// linear in world depth. Under perspective (spot + point) ndc.z is nonlinear,
// so a fixed NDC bias balloons into a world offset that grows with distance —
// at a few metres it lifts the receiver clean off its caster and punches a lit
// "hole"/donut out of the contact shadow directly under a mesh. Referencing the
// bias through this gradient keeps it a fixed small world offset at any range.
// For an orthographic projection (w-row = 0, clip.w constant) it collapses to a
// constant, so the directional path keeps its existing distance-invariant
// behaviour — now expressed, like every other path, in world metres.
fn ndc_depth_gradient(vp: mat4x4<f32>, clip_z: f32, clip_w: f32) -> f32 {
    let row2 = vec3<f32>(vp[0][2], vp[1][2], vp[2][2]);
    let row3 = vec3<f32>(vp[0][3], vp[1][3], vp[2][3]);
    let g = (row2 * clip_w - row3 * clip_z) / max(clip_w * clip_w, 1e-8);
    return length(g);
}

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

    // Cube face resolution (square). World-size of one texel uses this both
    // here (central receiver bias) and in the Soft/PCSS tap loops.
    let cube_face_res = f32(textureDimensions(shadow_cube_2d_array, 0).x);

    // Receiver depth bias, expressed in WORLD space and converted to NDC.z
    // through the local perspective depth gradient `grad = d(ndc_z)/d(view_depth)`.
    //
    // Why world-space: NDC.z is nonlinear under perspective, so a *constant*
    // NDC subtraction (what this used to do) maps to a world offset that grows
    // with distance — at a few metres the authored `depth_bias` ballooned to
    // ~10+ cm and lifted the receiver clean off its caster, punching the lit
    // "hole" out of the contact shadow directly under a mesh. Multiplying a
    // world-space push-back by `grad` keeps it a fixed small offset at any
    // light distance. `desc.bias_params.x` (`depth_bias`) is therefore in
    // METRES on this path; the `n_dot_dir` floor at 0.05 slope-scales it
    // (grazing surfaces need more) without running away as `n_dot_dir → 0`.
    // The per-texel quantization slack (`texel_world * kernel_slack`) is folded
    // into the same world push-back — both are world distances through one `grad`.
    let grad = (range / (range - near)) * near
        / max(view_depth * view_depth, near * near);
    let texel_world = 2.0 * view_depth / cube_face_res;
    let n_dot_dir = abs(dot(dir, world_normal));
    let world_bias = desc.bias_params.x / max(n_dot_dir, 0.05)
        + texel_world * desc.cascade_info.x;
    let ref_depth = clamp(ndc_z, 0.0, 1.0) - world_bias * grad;
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

    // Per-pixel phase for the Vogel disc (IGN hash on world pos, so adjacent
    // receivers sample rotated kernels and don't share a pattern). sin/cos
    // hoisted once — `vogel_tap` only rotates + scales per tap.
    let angle = pcss_disk_angle(
        biased_pos.xz * 137.0 + vec2<f32>(biased_pos.y * 31.0, biased_pos.y * 17.0),
    );
    let sin_p = sin(angle);
    let cos_p = cos(angle);

    // Per-light Vogel tap budget (descriptor extra_params.x). Soft/PCSS use `n`;
    // the blocker search uses a smaller `n_blocker`.
    let n = shadow_tap_count(desc.extra_params.x);
    let n_blocker = shadow_blocker_count(n);

    if hardness < 1.5 {
        // Soft — fixed-width Vogel disc, ~15 cm world radius, `n` taps (the
        // per-light `shadow_samples` budget); over a clump-free Vogel set this
        // resolves the soft edge smoothly with no rotation speckle.
        // World-space disc radius. Base 0.15 m at `pcss_penumbra_scale == 1`;
        // the per-light knob (bias_params.w) is the user's softness control,
        // shared with PCSS so one slider governs both modes for point lights too.
        let SOFT_WORLD_RADIUS: f32 = 0.15 * max(desc.bias_params.w, 0.0);
        let rsqrt_n = inverseSqrt(f32(n));
        var sum = 0.0;
        for (var i = 0u; i < n; i = i + 1u) {
            let off = vogel_tap(i, rsqrt_n, sin_p, cos_p) * SOFT_WORLD_RADIUS;
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
            // World-space receiver push-back → NDC.z via the local perspective
            // gradient (see the central block above). `depth_bias` is metres and
            // slope-scaled; the per-texel quantization slack (`world_per_texel =
            // 2·view_depth / res`, one texel can't bridge a real occluder gap)
            // rides the same `tap_grad`, so neither term explodes with distance.
            let tap_grad = (range / (range - near)) * near
                / max(tap_view_depth * tap_view_depth, near * near);
            let tap_texel_world = 2.0 * tap_view_depth / cube_face_res;
            let tap_world_bias = desc.bias_params.x / max(tap_n_dot_dir, 0.05)
                + tap_texel_world * desc.cascade_info.x;
            let tap_ref = clamp(tap_ndc_z, 0.0, 1.0) - tap_world_bias * tap_grad;
            sum += textureSampleCompareLevel(
                shadow_cube_array,
                shadow_cube_sampler,
                tap_dir,
                slot,
                tap_ref,
            );
        }
        return sum / f32(n);
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

    // Vogel blocker search (`n_blocker` taps). The averaged blocker depth
    // sets the penumbra WIDTH, so per-pixel variance here turns into a noisy
    // penumbra-radius field — a second noise source on top of the PCF below.
    // A clump-free Vogel set + more taps keeps the width estimate smooth.
    var blocker_sum = 0.0;
    var blocker_count = 0u;
    let blocker_rsqrt_n = inverseSqrt(f32(n_blocker));
    for (var i = 0u; i < n_blocker; i = i + 1u) {
        let off = vogel_tap(i, blocker_rsqrt_n, sin_p, cos_p) * pcss_search_world_radius;
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
    if blocker_count == n_blocker {
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
    let pcss_rsqrt_n = inverseSqrt(f32(n));
    for (var i = 0u; i < n; i = i + 1u) {
        let off = vogel_tap(i, pcss_rsqrt_n, sin_p, cos_p) * penumbra_world_radius;
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
        // World-space receiver push-back → NDC.z via the local perspective
        // gradient (see the central block + Soft path). `depth_bias` is metres
        // and slope-scaled; the per-texel quantization slack is a ONE-texel
        // footprint, independent of the (up to ~1 m) penumbra radius, so a wide
        // kernel no longer leaks the umbra and neither term grows with distance.
        let tap_grad = (range / (range - near)) * near
            / max(tap_view_depth * tap_view_depth, near * near);
        let tap_texel_world = 2.0 * tap_view_depth / cube_face_res;
        let tap_world_bias = desc.bias_params.x / max(tap_n_dot_dir, 0.05)
            + tap_texel_world * desc.cascade_info.x;
        let tap_ref = clamp(tap_ndc_z, 0.0, 1.0) - tap_world_bias * tap_grad;
        sum += textureSampleCompareLevel(
            shadow_cube_array,
            shadow_cube_sampler,
            tap_dir,
            slot,
            tap_ref,
        );
    }
    return sum / f32(n);
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
    // World-referenced depth bias (metres → NDC.z via the projection gradient).
    // For the orthographic cascade projection this gradient is constant, so this
    // is the same distance-invariant bias as before, just in world units — see
    // `ndc_depth_gradient`.
    let depth_grad = ndc_depth_gradient(desc.view_projection, clip.z, clip.w);
    let ref_depth = ndc.z - desc.bias_params.x * depth_grad;
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
        // Soft — fixed 16-tap rotated Poisson. See the matching
        // comment in `sample_shadow_cube`'s Soft branch — tapering
        // here introduced visible banding on large smooth receivers
        // (e.g. a floor plane under a directional light). The
        // PCSS branch below still tapers; the Soft path is fixed.
        let world_per_texel = max(desc.cascade_info.y, 1e-4);
        // World-unit penumbra → texel kernel below; scale-invariant. Base 0.12 m
        // at `pcss_penumbra_scale == 1`; the per-light knob (bias_params.w) is
        // the user's softness control, shared with PCSS. Fixed-width (no blocker
        // search), so keep the base modest — it does not narrow toward contact.
        let soft_world_radius = 0.12 * max(desc.bias_params.w, 0.0);
        let radius_texels = clamp(soft_world_radius / world_per_texel, 2.0, 10.0);
        let angle = pcss_disk_angle(
            biased_pos.xz * 137.0 + vec2<f32>(biased_pos.y * 31.0, biased_pos.y * 17.0),
        );
        let sin_a = sin(angle);
        let cos_a = cos(angle);
        let n = shadow_tap_count(desc.extra_params.x);
        let rsqrt_n = inverseSqrt(f32(n));
        var sum = 0.0;
        for (var i = 0u; i < n; i = i + 1u) {
            let off = vogel_tap(i, rsqrt_n, sin_a, cos_a) * radius_texels;
            sum += textureSampleCompareLevel(
                shadow_cascade_array, shadow_atlas_sampler,
                clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
                layer,
                ref_depth,
            );
        }
        return sum / f32(n);
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
    let n = shadow_tap_count(desc.extra_params.x);
    let n_blocker = shadow_blocker_count(n);
    let blocker_rsqrt_n = inverseSqrt(f32(n_blocker));
    let pcf_rsqrt_n = inverseSqrt(f32(n));
    let search_radius_texels = clamp(
        pcss_light_world_radius / world_per_texel_pcss,
        4.0,
        64.0,
    );
    // Fixed 16-tap blocker + PCF. The earlier tapered version
    // (`pcss_tap_count(ndc.z)`) showed clear ribbon/striping
    // artifacts on the canonical "robot on a floor under a
    // directional light" test — `ndc.z` is uncorrelated with
    // PCSS penumbra width, so fragments at `ndc.z ≈ 1` ended up
    // with 4 samples on a wide kernel, undersampling enough to
    // expose the rotated-Poisson disc as banding. Tapering is
    // parked here (and on the cube + 2D paths) until a quality-
    // preserving budget is worked out.
    var blocker_sum = 0.0;
    var blocker_count = 0u;
    let tile_min_px = vec2<i32>(tile_min * atlas_uv_to_texels);
    let tile_max_px = vec2<i32>(tile_max * atlas_uv_to_texels);
    for (var i = 0u; i < n_blocker; i = i + 1u) {
        let off = vogel_tap(i, blocker_rsqrt_n, sin_a, cos_a) * search_radius_texels;
        let sample_uv = atlas_uv + off * inv_atlas;
        let coord = vec2<i32>(sample_uv * atlas_uv_to_texels);
        let c = clamp(coord, tile_min_px, tile_max_px);
        let d = textureLoad(shadow_cascade_array, c, layer, 0);
        // Skip-self epsilon in WORLD space (~2 receiver texels) converted to
        // NDC.z via depth_grad. The old constant 0.0005 NDC is the same
        // perspective trap as the depth bias: harmless under the orthographic
        // cascade, but on the perspective spot path it grows to ~10 cm at a few
        // metres and rejects genuine near-contact occluders, collapsing
        // `blocker_count` so the PCSS early-out holes the contact shadow out to
        // fully lit. (For the ortho cascade this evaluates to ≈ the old value.)
        if d < ref_depth - 2.0 * world_per_texel_pcss * depth_grad {
            blocker_sum = blocker_sum + d;
            blocker_count = blocker_count + 1u;
        }
    }
    if blocker_count == 0u {
        return 1.0;
    }
    if blocker_count == n_blocker {
        return 0.0;
    }
    let avg_blocker = blocker_sum / f32(blocker_count);
    let light_size_texels = pcss_light_world_radius / world_per_texel_pcss;
    let penumbra_texels = clamp(
        (ref_depth - avg_blocker) * light_size_texels / max(avg_blocker, 1e-4),
        2.0,
        24.0,
    );
    // Wide PCSS kernels sample texels far from the fragment; on a sloped /
    // curved receiver the depth stored there differs by the surface slope and
    // self-shadows into acne. Scale the comparison bias with the kernel width so
    // wider penumbras get proportional slack — the softness hides the extra
    // peter-panning a near-contact (narrow-kernel) fragment would otherwise show.
    // World-referenced (× depth_grad) for the same reason as the base bias.
    let pcss_ref = ref_depth - desc.bias_params.x * penumbra_texels * 0.5 * depth_grad;
    var pcf_sum = 0.0;
    for (var i = 0u; i < n; i = i + 1u) {
        let off = vogel_tap(i, pcf_rsqrt_n, sin_a, cos_a) * penumbra_texels;
        pcf_sum = pcf_sum + textureSampleCompareLevel(
            shadow_cascade_array,
            shadow_atlas_sampler,
            clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
            layer,
            pcss_ref,
        );
    }
    return pcf_sum / f32(n);
}

// Sample a single shadow descriptor (cascade / spot / face). Returns
// `[0, 1]` visibility (1.0 = lit, 0.0 = fully shadowed).
//
// Hardness branches:
//   0.0 = Hard, 1-tap.
//   1.0 = Soft, 3x3 PCF.
//   2.0 = PCSS — blocker search + variable-kernel PCF.
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
    // World-referenced depth bias (metres → NDC.z via the projection gradient).
    // The spot projection is perspective, so this is what stops the authored
    // depth_bias from ballooning with distance and holing out contact shadows —
    // see `ndc_depth_gradient`.
    let depth_grad = ndc_depth_gradient(desc.view_projection, clip.z, clip.w);
    let ref_depth = ndc.z - desc.bias_params.x * depth_grad;
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
        // Penumbra half-width in WORLD units (converted to a texel kernel by
        // the divide below), so the perceived soft edge is identical regardless
        // of scene scale or which cascade resolves it — nothing here assumes a
        // particular scene size. The 0.12 m base is the default at
        // `pcss_penumbra_scale == 1`; that per-light knob (bias_params.w) is the
        // user's softness control, shared with PCSS so one slider governs both
        // modes. Unlike PCSS this kernel is fixed-width (no blocker search), so
        // it does not narrow toward contact — keep the base modest.
        let soft_world_radius = 0.12 * max(desc.bias_params.w, 0.0);
        // Clamp at 3 texels min (a too-tight kernel collapses to a
        // single 2×2 bilinear compare and the cascade-boundary blend
        // shows a "soft → razor" step). 20 texels max so the near
        // cascade doesn't waste kernel area where world_per_texel is
        // sub-millimetre.
        let radius_texels = clamp(soft_world_radius / world_per_texel, 2.0, 10.0);

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
        let n = shadow_tap_count(desc.extra_params.x);
        let rsqrt_n = inverseSqrt(f32(n));
        // Fixed 16 taps on the Soft path — see `sample_shadow_cube`'s
        // Soft branch for the full rationale. Tapering here banded
        // large smooth receivers; the PCSS branch below still
        // tapers because its variable-kernel PCF absorbs the noise.
        var sum = 0.0;
        for (var i = 0u; i < n; i = i + 1u) {
            let off = vogel_tap(i, rsqrt_n, sin_a, cos_a) * radius_texels;
            sum += textureSampleCompareLevel(
                shadow_atlas, shadow_atlas_sampler,
                clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
                ref_depth,
            );
        }
        return sum / f32(n);
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
    let n = shadow_tap_count(desc.extra_params.x);
    let n_blocker = shadow_blocker_count(n);
    let blocker_rsqrt_n = inverseSqrt(f32(n_blocker));
    let pcf_rsqrt_n = inverseSqrt(f32(n));
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

    // Fixed 16-tap blocker + PCF. Same rationale as the cascade-
    // array PCSS path: tapering by `ndc.z` undersamples wide
    // penumbras and shows as visible disc-rotation banding.
    var blocker_sum = 0.0;
    var blocker_count = 0u;
    let tile_min_px = vec2<i32>(tile_min * atlas_uv_to_texels);
    let tile_max_px = vec2<i32>(tile_max * atlas_uv_to_texels);
    for (var i = 0u; i < n_blocker; i = i + 1u) {
        let off = vogel_tap(i, blocker_rsqrt_n, sin_a, cos_a) * search_radius_texels;
        let sample_uv = atlas_uv + off * inv_atlas;
        let coord = vec2<i32>(sample_uv * atlas_uv_to_texels);
        // Clamp to the cascade's own tile so the blocker search
        // doesn't read from an adjacent cascade's depth values.
        let c = clamp(coord, tile_min_px, tile_max_px);
        let d = textureLoad(shadow_atlas, c, 0);
        // Skip-self epsilon in WORLD space (~2 receiver texels) via depth_grad —
        // see the matching cascade-array note. The old constant 0.0005 NDC grows
        // to ~10 cm of world depth at a few metres on this perspective path and
        // rejected near-contact occluders, holing the contact shadow.
        if d < ref_depth - 2.0 * world_per_texel_pcss * depth_grad {
            blocker_sum = blocker_sum + d;
            blocker_count = blocker_count + 1u;
        }
    }
    if blocker_count == 0u {
        return 1.0; // fully lit fast path
    }
    if blocker_count == n_blocker {
        // Every blocker-search sample was below the receiver's
        // biased depth — the receiver is deep inside the umbra
        // and the second 16-tap PCF would average to ≈ 0
        // anyway. Skip it.
        return 0.0;
    }
    let avg_blocker = blocker_sum / f32(blocker_count);
    // Classic PCSS penumbra: `(d_receiver − d_blocker) · light_size /
    // d_blocker`, but with light_size expressed in *world units* via
    // `world_per_texel`. The clamps keep the kernel between "more
    // than `Soft`" (4 texels) and "still affordable" (40 texels —
    // the 16-tap loop amortises hardware bilinear so this is fine).
    let light_size_texels = pcss_light_world_radius / world_per_texel_pcss;
    let penumbra_texels = clamp(
        (ref_depth - avg_blocker) * light_size_texels / max(avg_blocker, 1e-4),
        2.0,
        24.0,
    );
    // Wide PCSS kernels sample texels far from the fragment; on a sloped /
    // curved receiver the depth stored there differs by the surface slope and
    // self-shadows into acne. Scale the comparison bias with the kernel width so
    // wider penumbras get proportional slack — the softness hides the extra
    // peter-panning a near-contact (narrow-kernel) fragment would otherwise show.
    // The slack is the user's own `depth_bias` (bias_params.x) times the kernel
    // radius, so it inherits the per-light tuning instead of a fresh constant.
    // World-referenced (× depth_grad) for the same reason as the base bias.
    let pcss_ref = ref_depth - desc.bias_params.x * penumbra_texels * 0.5 * depth_grad;
    var pcf_sum = 0.0;
    for (var i = 0u; i < n; i = i + 1u) {
        let off = vogel_tap(i, pcf_rsqrt_n, sin_a, cos_a) * penumbra_texels;
        pcf_sum = pcf_sum + textureSampleCompareLevel(
            shadow_atlas,
            shadow_atlas_sampler,
            clamp(atlas_uv + off * inv_atlas, tile_min, tile_max),
            pcss_ref,
        );
    }
    return pcf_sum / f32(n);
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

{% endif %}{# end needs_shadow_sampling — shadow sampling functions #}
