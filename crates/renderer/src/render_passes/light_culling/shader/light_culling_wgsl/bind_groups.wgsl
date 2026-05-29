// Light culling pass bind groups.
//
// Single bind group:
//   0: camera_raw       — uniform, view matrix + viewport for screen-tile reconstruction.
//   1: cull_params      — uniform, per-frame tile/slice/capacity/near-far config.
//   2: lights_info      — uniform `LightsInfoPacked`.
//   3: lights           — uniform `array<LightPacked, MAX_PUNCTUAL_LIGHTS>`.
//   4: lights_storage   — storage RW (atomics), merged per-mesh + per-froxel buffer (see layout below).
//   5: overflow_counter — storage RW (atomic), single u32 incremented per dropped index.
//   6: tile_lights      — storage RW (atomics), two-level Stage-A per-2D-tile candidate list.
//
// The per-froxel tail of `lights_storage` is laid out in
// `(cull_params.max_per_froxel_capacity + 1)`-u32 strides (the capacity
// is a runtime field so the Phase 1D auto-grow path can bump it without
// recompiling):
//   stride = cull_params.max_per_froxel_capacity + 1
//   slot 0:           per-froxel count (atomic)
//   slots 1..1+count: light indices (atomic-stored)
//
// Merging counts + indices into one storage binding keeps the consumer
// (transparent / opaque-oversized) shaders under WebGPU's
// `maxStorageBuffersPerShaderStage = 10` baseline (those passes already
// bind 9 storage buffers — see `crates/renderer/src/lib.rs:332` for the
// budget).

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

// Layout kept in lockstep with the shared `lights.wgsl` definition (the
// shading passes read the `directional` indices; the cull only needs
// `data.x`, but the struct must match the 48-byte uniform buffer).
struct LightsInfoPacked {
    data: vec4<u32>,  // .x = n_lights; .y/.z = IBL mip counts; .w = n_directional
    directional: array<vec4<u32>, 2>,  // packed indices of the ≤8 directional lights
};

// Per-frame light-culling parameters. Written via writeBuffer at the top
// of every frame so the WGSL doesn't have to derive tile_x / tile_y / near /
// far from camera matrices.
struct CullParams {
    tiles_x: u32,                       // ceil(viewport_w / TILE_PIXEL_SIZE)
    tiles_y: u32,                       // ceil(viewport_h / TILE_PIXEL_SIZE)
    viewport_w: u32,                    // viewport width in pixels
    viewport_h: u32,                    // viewport height in pixels
    mesh_indices_capacity_u32: u32,     // head-region length in lights_storage; the cull pass
                                        // writes per-froxel data at offsets ≥ this value, and
                                        // consumer shaders compute the per-pixel froxel base
                                        // by adding it to the per-froxel-stride offset.
    max_per_froxel_capacity: u32,       // per-froxel light-index budget. Auto-grow path
                                        // doubles this without recompiling.
    tile_light_capacity: u32,           // per-2D-tile candidate budget (Stage-A `tile_lights`
                                        // stride = tile_light_capacity + 1). Grows toward the
                                        // live punctual-light count; runtime so no recompile.
    z_near: f32,                        // camera near plane (view-space, positive)
    z_far: f32,                         // camera far plane (view-space, positive)
    log_far_over_near: f32,             // precomputed log(z_far / z_near)
    debug_light_heatmap: u32,           // 0 = normal shading; 1 = output a per-pixel
                                        // applied-light-count heatmap (debug only)
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var<uniform> cull_params: CullParams;
@group(0) @binding(2) var<uniform> lights_info: LightsInfoPacked;
@group(0) @binding(3) var<uniform> lights: array<LightPacked, {{ max_punctual_lights }}u>;
// `lights_storage`: combined per-mesh + per-froxel u32 array. The cull
// pass writes per-froxel data at offsets ≥ `cull_params.mesh_indices_capacity_u32`;
// the head region is populated by `MeshLightIndicesGpu` on the CPU
// (we don't touch it from the cull shader). Declared as
// `array<atomic<u32>>` so the per-froxel atomic count + atomic index
// stores compile cleanly.
@group(0) @binding(4) var<storage, read_write> lights_storage: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> overflow_counter: atomic<u32>;
// `tile_lights`: two-level cull Stage-A output. One slice per 2D screen
// tile (`tiles_x * tiles_y`), each `cull_params.tile_light_capacity + 1`
// u32 wide (slot 0 = atomic count, slots 1.. = candidate light indices).
// The stride is a *runtime* value — `tile_light_capacity` is grown on the
// CPU to the live punctual count (clamped to MAX_PUNCTUAL_LIGHTS) and the
// buffer is reallocated to match, so a tile can't overflow. `cs_tile`
// writes it (side-plane test, no Z); `cs_main` reads each froxel's tile
// slice and applies only the Z-test.
@group(0) @binding(6) var<storage, read_write> tile_lights: array<atomic<u32>>;
