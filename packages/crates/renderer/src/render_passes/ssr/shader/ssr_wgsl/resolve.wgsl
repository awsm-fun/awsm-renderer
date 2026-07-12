// SSR spatial resolve — the edge-aware denoise between trace and composite.
//
// The stochastic trace jitters its march phase per pixel (and rotates it per
// frame under temporal), so its raw output carries per-pixel noise and
// dithered hit/miss edges. Compositing that directly reads as fuzzy
// "caterpillar" edges on reflected detail and sparkle on glossy surfaces at
// grazing angles. This pass runs at the SSR target's own resolution (half- or
// full-res, same as the trace) and applies a 9-tap edge-aware disk filter:
// center + 8 taps on a golden-angle spiral of radius ~2.5 output texels, each
// weighted by a spatial gaussian × a depth-similarity term (the same
// edge-stopping form the composite's joint-bilateral upsample uses), so
// reflection energy never bleeds across geometry silhouettes.
//
// Input is the trace's reflection-ONLY premultiplied color (alpha = coverage);
// rgb AND coverage accumulate with the same weights and divide by the same
// weight sum, so the output stays correctly premultiplied with a fractional,
// smoothed coverage — the dithered hit/miss boundary becomes a soft edge that
// composites correctly through the existing additive blend.
//
// Ordering: trace → THIS → temporal accumulation (`ssr_wgsl/temporal.wgsl`,
// when enabled) → composite. The temporal pass consumes this pass's output,
// so its 3×3 neighborhood clamp operates on the denoised signal rather than
// the raw stochastic trace.

// CameraRaw + camera_from_raw (inv_proj for view-space depth linearization).
{% include "shared_wgsl/camera.wgsl" %}

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
// The raw SSR trace output (rgba16float, premultiplied rgb + coverage alpha).
@group(0) @binding(1) var src_tex: texture_2d<f32>;
// Full-res post-opaque depth — multisampled under MSAA, mirroring the trace's
// own depth binding (same buffer, same variant axis).
{% if multisampled_geometry %}
@group(0) @binding(2) var depth_tex: texture_depth_multisampled_2d;
{% else %}
@group(0) @binding(2) var depth_tex: texture_depth_2d;
{% endif %}
// Same-size resolved output; the composite reads THIS instead of the raw trace.
@group(0) @binding(3) var out_tex: texture_storage_2d<rgba16float, write>;

// Reconstruct VIEW-space position from a hardware depth sample at `uv`
// (NDC y flipped vs UV). Same convention as trace.wgsl's view_pos_from_depth.
fn view_pos_from_depth(uv: vec2<f32>, depth: f32, cam: Camera) -> vec3<f32> {
    let ndc = vec3<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth);
    let v = cam.inv_proj * vec4<f32>(ndc, 1.0);
    return v.xyz / v.w;
}

// Positive linear view-space depth (view looks down -Z, so +linear = -z).
fn linear_z(uv: vec2<f32>, depth: f32, cam: Camera) -> f32 {
    return -view_pos_from_depth(uv, depth, cam).z;
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_dims = textureDimensions(out_tex);
    let coords = vec2<i32>(gid.xy);
    if (coords.x >= i32(out_dims.x) || coords.y >= i32(out_dims.y)) {
        return;
    }
    let out_max = vec2<i32>(out_dims) - vec2<i32>(1, 1);
    let out_dims_f = vec2<f32>(out_dims);
    // UV is resolution-independent, so the full-res depth loads work whether
    // the SSR target is full- or half-res.
    let uv = (vec2<f32>(coords) + vec2<f32>(0.5)) / out_dims_f;
    let full_dims = vec2<f32>(textureDimensions(depth_tex));

    let cam = camera_from_raw(camera_raw);
    let center_depth = textureLoad(depth_tex, vec2<i32>(uv * full_dims), 0);

    // Sky: the trace wrote zero coverage here and there is no surface to
    // edge-compare against — write 0 and keep the additive composite a no-op.
    {% if reverse_z %}
    if (center_depth <= 0.0) {
    {% else %}
    if (center_depth >= 1.0) {
    {% endif %}
        textureStore(out_tex, coords, vec4<f32>(0.0, 0.0, 0.0, 0.0));
        return;
    }
    let z_center = linear_z(uv, center_depth, cam);

    // sigma: edge-stopping width in VIEW-space linear Z — 5% of the center
    // depth (scale-relative), floored at 1e-2 world units. Identical form to
    // the composite upsample's depth weight.
    let sigma = max(z_center * 0.05, 1e-2);

    // 8 taps on a golden-angle spiral disk, radius 2.5 output texels:
    // r_i = 2.5*sqrt((i+0.5)/8), angle_i = i * 2.3999632 rad. Precomputed.
    var tap_offsets = array<vec2<f32>, 8>(
        vec2<f32>(0.625000, 0.000000),
        vec2<f32>(-0.798225, 0.731240),
        vec2<f32>(0.122181, -1.392191),
        vec2<f32>(1.006111, 1.312294),
        vec2<f32>(-1.846338, -0.326591),
        vec2<f32>(1.749012, -1.112579),
        vec2<f32>(-0.585010, 2.176210),
        vec2<f32>(-1.115679, -2.148170),
    );
    // Spatial gaussian per tap: exp(-r_i^2 / (2 * 1.25^2)). Precomputed.
    var tap_gauss = array<f32, 8>(
        0.882497,
        0.687289,
        0.535261,
        0.416862,
        0.324652,
        0.252840,
        0.196912,
        0.153355,
    );

    // Center tap: gaussian(0) = 1, depth similarity = 1 by construction.
    // rgb AND coverage accumulate with the same weights (premultiplied color +
    // filtered fractional coverage composites correctly through the additive
    // blend).
    let center = textureLoad(src_tex, coords, 0);
    var sum = center;
    var sum_w = 1.0;

    // TRAVEL-SCALED radius: trace alpha carries coverage x travel fraction.
    // Contact reflections (travel ~0) stay tight (1x = 2.5px disk); far
    // reflections widen up to 3.2x (~8px). This is the glossy-with-distance
    // falloff every production SSR has — and it is what buries the serrated
    // edges a mirror-sharp reflection of thin bright tubes produces (the
    // depth silhouette is aliased and grazing reflection stretch amplifies
    // it; no amount of exact tracing removes that, only filtering does).
    // Sample the local max travel so the widened kernel also covers the
    // miss-side of a reflection boundary.
    var travel = center.a;
    for (var i = 0; i < 8; i = i + 1) {
        let t4 = clamp(
            vec2<i32>(floor(vec2<f32>(coords) + vec2<f32>(0.5) + tap_offsets[i] * 1.6)),
            vec2<i32>(0, 0),
            out_max,
        );
        travel = max(travel, textureLoad(src_tex, t4, 0).a);
    }
    let radius_scale = 1.0 + travel * 2.2;

    for (var i = 0; i < 8; i = i + 1) {
        let tap = clamp(
            vec2<i32>(floor(vec2<f32>(coords) + vec2<f32>(0.5) + tap_offsets[i] * radius_scale)),
            vec2<i32>(0, 0),
            out_max,
        );
        // Full-res depth under this tap's output-texel center (like the
        // composite's per-tap depth fetch).
        let tap_uv = (vec2<f32>(tap) + vec2<f32>(0.5)) / out_dims_f;
        let tap_depth = textureLoad(depth_tex, vec2<i32>(tap_uv * full_dims), 0);
        // Sky taps have no meaningful linear Z (reverse-Z depth 0 reconstructs
        // non-finite) — skip them entirely rather than poisoning the sum.
        {% if reverse_z %}
        if (tap_depth <= 0.0) {
        {% else %}
        if (tap_depth >= 1.0) {
        {% endif %}
            continue;
        }
        let z_tap = linear_z(tap_uv, tap_depth, cam);
        // Depth-similarity edge weight (wgsl_validation pins this exact term).
        let dw = exp(-abs(z_tap - z_center) / sigma);
        let w = tap_gauss[i] * dw;
        sum = sum + textureLoad(src_tex, tap, 0) * w;
        sum_w = sum_w + w;
    }

    // Coverage-weighted divide: rgb and coverage share the one weight sum
    // (wgsl_validation pins this line). sum_w >= 1 (center tap), so no guard.
    textureStore(out_tex, coords, sum / sum_w);
}
