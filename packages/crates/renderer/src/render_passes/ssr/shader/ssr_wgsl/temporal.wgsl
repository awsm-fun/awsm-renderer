// SSR temporal accumulation — the dedicated history pass AFTER the spatial
// resolve (trace → resolve → THIS → composite).
//
// The old design accumulated history inside the trace, BEFORE the spatial
// resolve, with a bare depth-reproject and NO color clamp. Reflections move
// with different parallax than the surfaces they sit on, so under camera
// motion the reprojected history was stale and the unclamped blend
// (weight 0.85 ≈ 7-frame persistence) smeared it into multi-frame trails.
//
// This pass reads the spatially-resolved current frame (`ssr_resolved`),
// depth-reprojects into the previous frame's accumulated history, and — the
// anti-ghosting core — CLAMPS the history sample to the 3×3 neighborhood
// AABB (min/max of ssr_resolved rgb+coverage around the pixel). Stale history
// gets pulled to the current neighborhood's gamut, so trails die in 1-2
// frames instead of persisting for ~1/(1-weight). Off-screen / behind-camera
// reprojections (disocclusions) keep the fresh current value.
//
// Output goes to BOTH `ssr_final` (what the composite reads when temporal is
// on) and this frame's history slot (what next frame reprojects into). The
// history pair ping-pongs by frame parity, mirrored by the parity bind groups.

// CameraRaw + camera_from_raw (inv_proj / inv_view / prev_view_proj).
{% include "shared_wgsl/camera.wgsl" %}

// NOTE: this pass deliberately has NO spread gate (see the blend below) —
// mirror pixels accumulate exactly like glossy ones, converging the trace's
// per-frame mirror phase cycle. The "ssr-spread-gate" constant lives in
// ssr_wgsl/resolve.wgsl and shared_wgsl/lighting/brdf_pbr.wgsl only.

// Same 32-byte live-tuning uniform the trace binds; only `temporal_weight`
// is read here. Layout must match `struct SsrParams` in `ssr_wgsl/trace.wgsl`.
struct SsrParams {
    intensity: f32,
    max_distance: f32,
    thickness: f32,
    max_steps: f32,
    spread_cutoff: f32,
    edge_fade: f32,
    temporal_weight: f32,
    frame: f32,
};

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var<uniform> params: SsrParams;
// Full-res post-opaque depth — multisampled under MSAA, mirroring the trace's
// own depth binding (same buffer, same variant axis).
{% if multisampled_geometry %}
@group(0) @binding(2) var depth_tex: texture_depth_multisampled_2d;
{% else %}
@group(0) @binding(2) var depth_tex: texture_depth_2d;
{% endif %}
// This frame's spatially-resolved reflection (integer textureLoad only).
@group(0) @binding(3) var current_tex: texture_2d<f32>;
// Previous frame's accumulated history — the reprojected UV is fractional, so
// this is the one genuinely FILTERED fetch (linear sampler at binding 5).
@group(0) @binding(4) var history_prev_tex: texture_2d<f32>;
@group(0) @binding(5) var history_sampler: sampler;
// Accumulated output — `ssr_final`, the composite's source when temporal is on.
@group(0) @binding(6) var out_tex: texture_storage_2d<rgba16float, write>;
// This frame's history slot (next frame's reprojection source).
@group(0) @binding(7) var history_curr_tex: texture_storage_2d<rgba16float, write>;
// Material-owned reflection descriptor (single-sample, FULL-res; same texture
// the trace reads at binding 6). Declared for bind-group-layout parity but no
// longer read: the blend is spread-UNGATED (mirror pixels accumulate their
// per-frame phase cycle like glossy pixels accumulate their jitter).
@group(0) @binding(8) var reflection_descriptor_tex: texture_2d<f32>;

// Reconstruct VIEW-space position from a hardware depth sample at `uv`
// (NDC y flipped vs UV). Same convention as trace.wgsl's view_pos_from_depth.
fn view_pos_from_depth(uv: vec2<f32>, depth: f32, cam: Camera) -> vec3<f32> {
    let ndc = vec3<f32>(uv.x * 2.0 - 1.0, (1.0 - uv.y) * 2.0 - 1.0, depth);
    let v = cam.inv_proj * vec4<f32>(ndc, 1.0);
    return v.xyz / v.w;
}

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_dims = textureDimensions(out_tex);
    let coords = vec2<i32>(gid.xy);
    if (coords.x >= i32(out_dims.x) || coords.y >= i32(out_dims.y)) {
        return;
    }
    // UV is resolution-independent, so the full-res depth loads work whether
    // the SSR target is full- or half-res.
    let uv = (vec2<f32>(coords) + vec2<f32>(0.5)) / vec2<f32>(out_dims);
    let full_dims = textureDimensions(depth_tex);
    let fcoords = vec2<i32>(uv * vec2<f32>(full_dims));

    let cam = camera_from_raw(camera_raw);
    let depth = textureLoad(depth_tex, fcoords, 0);

    // Sky: nothing to reflect from, no surface to reproject (reverse-Z depth 0
    // reconstructs non-finite). Zero both outputs so the composite is a no-op
    // and next frame's history carries no stale energy here.
    {% if reverse_z %}
    if (depth <= 0.0) {
    {% else %}
    if (depth >= 1.0) {
    {% endif %}
        textureStore(out_tex, coords, vec4<f32>(0.0, 0.0, 0.0, 0.0));
        textureStore(history_curr_tex, coords, vec4<f32>(0.0, 0.0, 0.0, 0.0));
        return;
    }

    let current = textureLoad(current_tex, coords, 0);

    // 3×3 neighborhood AABB (min/max of rgb + coverage) of the CURRENT
    // resolved frame — the gamut the reprojected history must fit into.
    let out_max = vec2<i32>(out_dims) - vec2<i32>(1, 1);
    var nb_min = current;
    var nb_max = current;
    for (var j = -1; j <= 1; j = j + 1) {
        for (var i = -1; i <= 1; i = i + 1) {
            if (i == 0 && j == 0) {
                continue;
            }
            let tap = clamp(coords + vec2<i32>(i, j), vec2<i32>(0, 0), out_max);
            let v = textureLoad(current_tex, tap, 0);
            nb_min = min(nb_min, v);
            nb_max = max(nb_max, v);
        }
    }

    // Depth reproject: reconstruct this pixel's WORLD position (same math as
    // the trace), project with the PREVIOUS frame's view-projection to find
    // where this surface was last frame. Off-screen / behind the previous
    // camera (prev_clip.w <= 0) = disocclusion → keep the fresh current value.
    var out_color = current;
    let p = view_pos_from_depth(uv, depth, cam);
    let world_pos = (cam.inv_view * vec4<f32>(p, 1.0)).xyz;
    let prev_clip = cam.prev_view_proj * vec4<f32>(world_pos, 1.0);
    if (prev_clip.w > 0.0) {
        let prev_ndc = prev_clip.xyz / prev_clip.w;
        let prev_uv = vec2<f32>(prev_ndc.x * 0.5 + 0.5, 1.0 - (prev_ndc.y * 0.5 + 0.5));
        if (prev_uv.x >= 0.0 && prev_uv.x <= 1.0 && prev_uv.y >= 0.0 && prev_uv.y <= 1.0) {
            let hist = textureSampleLevel(history_prev_tex, history_sampler, prev_uv, 0.0);
            // Neighborhood clamp — the anti-ghosting core (wgsl_validation
            // pins this line): stale history is pulled to the current
            // neighborhood's AABB, so camera-motion trails die in 1-2 frames.
            // UNCONDITIONAL neighborhood clamp (wgsl_validation pins this):
            // stale history is always pulled to the current neighborhood's
            // gamut, so ghosting dies in 1-2 frames whatever moved — camera,
            // object, or authoring edit. Mirror pixels are deterministic in
            // the trace (bilinear scene depth, fixed phase), so they neither
            // need nor fight the clamp; glossy pixels' per-pixel jitter is
            // sub-neighborhood by construction and converges within the
            // AABB.
            let hist_clamped = clamp(hist, nb_min, nb_max);
            out_color = mix(
                current,
                hist_clamped,
                params.temporal_weight,
            );
        }
    }

    textureStore(out_tex, coords, out_color);
    // Persist the accumulated result so next frame's reprojection reads it.
    textureStore(history_curr_tex, coords, out_color);
}
