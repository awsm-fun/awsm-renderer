// Light culling pass bind groups.
//
// Single bind group:
//   0: camera_raw       — uniform, view matrix + viewport for screen-tile reconstruction.
//   1: cull_params      — uniform, per-frame tile/slice/capacity/near-far config.
//   2: lights_info      — uniform `LightsInfoPacked` (the canonical helper struct from
//                          shared/lights.wgsl). The cull only consumes `.data.x` (`n_lights`),
//                          but binds the same buffer the shading passes use so the
//                          render-side `BindGroupCreate::LightsInfoCreate` event covers it.
//   3: lights           — uniform `array<LightPacked, MAX_PUNCTUAL_LIGHTS>`. Same physical
//                          buffer as the opaque/transparent main pass; same name so the
//                          shared `get_light(i)` helper resolves it.
//   4: froxel_counts    — storage RW (atomics), per-froxel count of appended indices.
//   5: froxel_indices   — storage RW, flat `[froxel_count * max_per_froxel_capacity]` of u32 light indices.
//   6: overflow_counter — storage RW (atomic), single u32 incremented per dropped index.
//
// Each froxel's index slice lives at
// `froxel_indices[froxel_idx * MAX_PER_FROXEL_CAPACITY .. + count]`. `offset` is implicit
// — every froxel has the same capacity, so we don't need a prefix scan. Saturation
// increments `overflow_counter` and the CPU's auto-grow readback bumps the budget on
// the next frame.

/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

// LightPacked / LightsInfoPacked — kept in lockstep with
// `shared_wgsl/lighting/lights.wgsl`. The shared file is full of
// shading machinery (BRDF, shadow sampling, prefix walks) the cull
// pass doesn't need; copying just the two struct decls keeps the
// shader template free of unused template flags
// (`use_mesh_light_slices` / `has_lighting_*` / `shadows_enabled`).
struct LightPacked {
    pos_range: vec4<f32>,         // .xyz = position, .w = range
    dir_inner: vec4<f32>,         // .xyz = direction, .w = inner-cone cos
    color_intensity: vec4<f32>,   // .xyz = color, .w = intensity
    kind_outer_pad: vec4<f32>,    // .x = kind (1=Dir, 2=Point, 3=Spot), .y = outer-cone cos, .z = shadow_index (bitcast u32), .w = pad
};

struct LightsInfoPacked {
    data: vec4<u32>,  // .x = n_lights; .y/.z = IBL mip counts; .w = pad
};

// Per-frame light-culling parameters. Written via writeBuffer at the top
// of every frame so the WGSL doesn't have to derive tile_x / tile_y / near /
// far from camera matrices.
struct CullParams {
    tiles_x: u32,            // ceil(viewport_w / TILE_PIXEL_SIZE)
    tiles_y: u32,            // ceil(viewport_h / TILE_PIXEL_SIZE)
    viewport_w: u32,         // viewport width in pixels (for side-plane reconstruction)
    viewport_h: u32,         // viewport height in pixels
    z_near: f32,             // camera near plane (view-space, positive)
    z_far: f32,              // camera far plane (view-space, positive)
    log_far_over_near: f32,  // precomputed log(z_far / z_near); reused per froxel
    _pad: f32,
};

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var<uniform> cull_params: CullParams;
@group(0) @binding(2) var<uniform> lights_info: LightsInfoPacked;
@group(0) @binding(3) var<uniform> lights: array<LightPacked, {{ max_punctual_lights }}u>;
@group(0) @binding(4) var<storage, read_write> froxel_counts: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> froxel_indices: array<u32>;
@group(0) @binding(6) var<storage, read_write> overflow_counter: atomic<u32>;
