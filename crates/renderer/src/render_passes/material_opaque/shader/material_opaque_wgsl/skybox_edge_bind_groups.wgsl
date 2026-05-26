// Bind-group declarations for the skybox_edge_resolve compute shader.
//
// Needs: skybox texture + sampler, camera uniform, edge buffer
// (read-write — writes accumulator slots; read-only counters).

struct EdgeIndirectArgsSky {
    workgroup_count_x: u32,
    workgroup_count_y: u32,
    workgroup_count_z: u32,
    _pad: u32,
};

struct EdgeBuffersSky {
    edge_count: u32,
    edge_overflow_count: u32,
    _pad_counters: vec2<u32>,
    final_blend_args: EdgeIndirectArgsSky,
    skybox_edge_args: EdgeIndirectArgsSky,
    {% for entry in bucket_entries %}
    {{ entry.args_field() }}_edge: EdgeIndirectArgsSky,
    {% endfor %}
    data: array<u32>,
};

@group(0) @binding(0) var<storage, read_write> edge_buffers: EdgeBuffersSky;

struct EdgeBufferLayoutSky {
    max_edge_budget: u32,
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

// Camera uniform (sample_skybox needs camera.view_proj_inverse).
@group(0) @binding(2) var<uniform> camera_raw: array<vec4<f32>, 16>;

@group(0) @binding(3) var skybox_tex: texture_cube<f32>;
@group(0) @binding(4) var skybox_sampler: sampler;
