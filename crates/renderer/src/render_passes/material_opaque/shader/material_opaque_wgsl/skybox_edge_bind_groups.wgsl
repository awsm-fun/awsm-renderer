// Bind-group declarations for the skybox_edge_resolve compute shader.
//
// Needs: skybox texture + sampler, camera uniform, edge buffer
// (read-write — writes accumulator slots; read-only counters).

// Include the shared CameraRaw + Camera + camera_from_raw helpers
// here so the uniform binding below can reference `CameraRaw` and the
// compute shader's body can call `camera_from_raw(camera_raw)`. The
// compute file (skybox_edge_resolve.wgsl) intentionally does NOT
// re-include camera.wgsl — including it twice would redefine the
// CameraRaw struct.
/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

// data_buffer: small counter-mirror header + edge_to_xy + edge_slot_map
// + accumulator + sample lists. The skybox entry count lives in the
// header at `skybox_count_index` (supplied via the layout uniform).
@group(0) @binding(0) var<storage, read_write> edge_data: array<u32>;

struct EdgeBufferLayoutSky {
    max_edge_budget: u32,
    edge_count_index: u32,
    per_shader_count_base: u32,
    skybox_count_index: u32,
    edge_to_xy_base: u32,
    edge_slot_map_base: u32,
    accumulator_base: u32,
    {% for entry in bucket_entries %}
    {{ entry.args_field() }}_sample_list_base: u32,
    {% endfor %}
    skybox_sample_list_base: u32,
    sample_entries_per_bucket: u32,
};

@group(0) @binding(1) var<uniform> edge_layout: EdgeBufferLayoutSky;

// Camera uniform (sample_skybox needs camera.inv_view_proj). Declared
// via the canonical `CameraRaw` struct from shared_wgsl/camera.wgsl —
// that file is included by skybox_edge_resolve.wgsl above so the
// struct is in scope here.
@group(0) @binding(2) var<uniform> camera_raw: CameraRaw;

@group(0) @binding(3) var skybox_tex: texture_cube<f32>;
@group(0) @binding(4) var skybox_sampler: sampler;
