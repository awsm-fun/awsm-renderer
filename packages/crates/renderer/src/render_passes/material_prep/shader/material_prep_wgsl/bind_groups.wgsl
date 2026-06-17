// Bind group declarations for the material prep compute pass (Plan B,
// docs/plans/deferred-shared-prep-pass.md). Layout must stay in lockstep with
// material_prep/bind_group.rs (added in the pipeline-wiring sub-stage).
//
// Inputs (read): visibility texture (triangle id + meta offset) + barycentric
// texture from the geometry pass, the merged geometry pool (`visibility_data`),
// and per-mesh metadata. Outputs (storage-write): interpolated UV0 + vertex
// color — the geometry-pool-fetch-heavy attributes the slim per-material shader
// would otherwise recompute. World position is NOT materialized (decision #2:
// the slim shader keeps the cheap depth-unprojection). Shadow visibility + edge
// outputs arrive in stages 3 / 5.

// Per-mesh metadata struct (defined here so the binding below can reference it;
// included once — the compute half references it after concatenation).
{% include "shared_wgsl/material_mesh_meta.wgsl" %}

// Visibility buffer (triangle id + meta offset), from the geometry pass.
@group(0) @binding(0) var visibility_data_tex: {% if multisampled_geometry %}texture_multisampled_2d<u32>{% else %}texture_2d<u32>{% endif %};
// Barycentric (RG = u16 fixed-point weights; BA = instance id).
@group(0) @binding(1) var barycentric_tex: {% if multisampled_geometry %}texture_multisampled_2d<u32>{% else %}texture_2d<u32>{% endif %};
// Merged geometry pool (positions / indices / vertex attributes), as f32 words.
@group(0) @binding(2) var<storage, read> visibility_data: array<f32>;
// Per-mesh metadata (offsets, strides, set indices).
@group(0) @binding(3) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;

// Materialized outputs (storage-write ARRAYS — one layer per UV / color set).
@group(0) @binding(4) var uv_out: texture_storage_2d_array<rg32float, write>;
@group(0) @binding(5) var vcolor_out: texture_storage_2d_array<rgba32float, write>;

{% if shadows %}
// ── Plan B Stage 3b — per-pixel shadow-visibility computation ────────────────
// The prep pass walks the canonical froxel order (froxel_walk.wgsl SSOT) and,
// for each shadowed light, samples its shadow map (sample_shadow_directional)
// EXACTLY as `apply_lighting_per_froxel` does, packing 4 visibility slots per
// Rgba8unorm texel (slot j -> layer j/4, channel j%4). INERT: the buffer is
// written but not yet read (lighting still samples shadows inline until Stage 4).
//
// Includes below pull in the shared lighting + shadow-sampling machinery. The
// include ORDER matters: each file references types/globals declared before it.

// CameraRaw + camera_from_raw — needed for depth->world_pos + view_z, and the
// `camera_raw` uniform `apply_sscs` reads directly.
{% include "shared_wgsl/camera.wgsl" %}

// Light data STRUCTS (LightPacked / LightsInfoPacked / Light / LightSample /
// IblInfo / LightsInfo) — bind-group ABI for the group(1) light bindings below.
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

// group(0) shadow-feature additions: depth + normal/tangent + camera + the
// packed shadow-visibility output array (Rgba8unorm, ceil(K/4) layers).
{% if multisampled_geometry %}
    @group(0) @binding(6) var depth_tex: texture_depth_multisampled_2d;
    @group(0) @binding(7) var normal_tangent_tex: texture_multisampled_2d<f32>;
{% else %}
    @group(0) @binding(6) var depth_tex: texture_depth_2d;
    @group(0) @binding(7) var normal_tangent_tex: texture_2d<f32>;
{% endif %}
@group(0) @binding(8) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(9) var shadow_visibility_out: texture_storage_2d_array<rgba8unorm, write>;

// group(1) — lights (mirror material_opaque_wgsl/bind_groups.wgsl).
@group(1) @binding(0) var<uniform> lights_info: LightsInfoPacked;
@group(1) @binding(1) var<uniform> lights: array<LightPacked, 1024>;
@group(1) @binding(2) var<storage, read> lights_storage: array<u32>;
@group(1) @binding(3) var<uniform> cull_params: CullParams;

// Light accessors (get_light / get_n_directional / get_directional_light_index /
// light_sample / shadow_normal_toward_light) — need the group(1) globals above.
{% include "shared_wgsl/lighting/light_access.wgsl" %}

// Froxel addressing + light-walk enumeration order (SSOT) — needs cull_params +
// lights_storage + the `froxel_slice_count` template var.
{% include "shared_wgsl/lighting/froxel_walk.wgsl" %}

// Shadow bind group (group {{ shadow_group_index }}) + sampling functions
// (sample_shadow_directional / apply_sscs / SHADOW_INDEX_NONE /
// debug_cascade_tint). apply_sscs reads depth_tex + camera_raw (declared above).
{% include "shared_wgsl/shadow/bind_groups.wgsl" %}
{% endif %}
