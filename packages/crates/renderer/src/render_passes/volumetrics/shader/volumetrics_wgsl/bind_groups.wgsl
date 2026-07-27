// Bind group declarations for the volumetrics compute pass. Layout must stay
// in lockstep with volumetrics/bind_group.rs.
//
// Include ORDER matters: each file references types/globals declared before it.
// This mirrors material_prep_wgsl/bind_groups.wgsl, which is the other compute
// pass that binds both the lights group and the shadow group.

// Shared math (`inverse_square`, used by `light_sample`'s distance falloff).
{% include "shared_wgsl/math.wgsl" %}

// CameraRaw + camera_from_raw — the froxel→world reconstruction needs the
// inverse matrices, and the shared shadow include reads `camera_raw` directly.
{% include "shared_wgsl/camera.wgsl" %}

// Light data STRUCTS (LightPacked / LightsInfoPacked / Light / LightSample /
// IblInfo / LightsInfo) — the bind-group ABI for the group(1) bindings below.
{% include "shared_wgsl/lighting/light_access_types.wgsl" %}

// `CullParams` is declared per-pass (NOT shared); copied verbatim from
// material_opaque_wgsl/bind_groups.wgsl. Must stay byte-aligned with the cull
// pass's `light_culling_wgsl/bind_groups.wgsl` (froxel_walk.wgsl reads it).
struct CullParams {
    tiles_x: u32,
    tiles_y: u32,
    viewport_w: u32,
    viewport_h: u32,
    mesh_indices_capacity_u32: u32,
    max_per_froxel_capacity: u32,
    tile_light_capacity: u32,
    z_near: f32,
    z_far: f32,
    log_far_over_near: f32,
    debug_light_heatmap: u32,
    debug_view_mode: u32,
    debug_wireframe: u32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
};

// The medium + grid description. Mirrors `VolumetricParams` in render_pass.rs.
// The medium fields are the SAME numbers the analytic haze uses — the two
// paths render one medium, integrated differently.
struct VolumetricParamsRaw {
    scattering_color: vec3<f32>,
    density: f32,
    base_height: f32,
    height_falloff: f32,
    // Henyey-Greenstein g, pre-clamped on the CPU side away from ±1.
    anisotropy: f32,
    slice_count: f32,
    grid_size: vec2<u32>,
    // The volume's OWN depth range, sliced UNIFORMLY across it — see
    // `froxel_slice_view_z` for why this one is not the light grid's
    // exponential mapping. Shares the light grid's near plane; the far plane
    // is `volumetric_distance`, and past it the composite hands off to the
    // analytic medium.
    z_near: f32,
    z_far: f32,
    // Per-frame sub-froxel sample offset, in froxel units, each component in
    // [-0.5, 0.5). Zero when temporal is off — one froxel, one centre sample.
    jitter: vec3<f32>,
    // Fraction of the REPROJECTED history kept. 0 disables the blend outright,
    // which is what the first frame after an enable/resize uses (there is no
    // history yet, and blending against a cleared volume would fade the medium
    // in over ~20 frames).
    history_blend: f32,
};

// ── group(0) — volume + params ───────────────────────────────────────────────
@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var<uniform> volumetric_params: VolumetricParamsRaw;
// Read side. `integrate` samples the scatter volume here. `inject` binds
// either a 1×1×1 dummy (no temporal) or LAST frame's scatter volume, which is
// a different texture from the one it writes — the two-usages-in-one-scope
// rule is per subresource and keys off what the bind group declares.
@group(0) @binding(2) var src_volume: texture_3d<f32>;
@group(0) @binding(3) var dst_volume: texture_storage_3d<rgba16float, write>;
{% if temporal %}
// Trilinear. Reprojection lands between froxels by construction — the whole
// point is that this frame's jittered samples fall where last frame's did not.
@group(0) @binding(4) var history_sampler: sampler;
{% endif %}

// ── group(1) — lights (mirrors material_prep / material_opaque) ──────────────
@group(1) @binding(0) var<uniform> lights_info: LightsInfoPacked;
@group(1) @binding(1) var<uniform> lights: array<LightPacked, 1024>;
@group(1) @binding(2) var<storage, read> lights_storage: array<u32>;
@group(1) @binding(3) var<uniform> cull_params: CullParams;

// Light accessors (get_light / get_n_directional / get_directional_light_index /
// light_sample) — need the group(1) globals above.
{% include "shared_wgsl/lighting/light_access.wgsl" %}

// Froxel addressing + the canonical light-walk order (SSOT) — needs cull_params
// + lights_storage + the `froxel_slice_count` template var. Calling this with
// the froxel CENTRE pixel is what makes the volume enumerate exactly the lights
// the surfaces in that column enumerate.
{% include "shared_wgsl/lighting/froxel_walk.wgsl" %}

// ── group(2) — shadows ───────────────────────────────────────────────────────
// The same include, the same `sample_shadow_descriptor`, the same atlas the
// surfaces sample. SSCS is compiled out (`sscs_available: false`): it is a
// screen-space *surface* contact term and this pass has no depth buffer.
{% include "shared_wgsl/shadow/bind_groups.wgsl" %}
