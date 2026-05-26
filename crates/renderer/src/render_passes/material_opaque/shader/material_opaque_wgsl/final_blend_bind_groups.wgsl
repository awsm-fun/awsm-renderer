// Bind-group declarations for the final_blend compositor.
//
// Only needs: the edge buffer (read-only counters + per-edge arrays +
// accumulator) and the opaque storage texture as the write target.

struct EdgeIndirectArgsRO {
    workgroup_count_x: u32,
    workgroup_count_y: u32,
    workgroup_count_z: u32,
    _pad: u32,
};

struct EdgeBuffersRO {
    edge_count: u32,
    edge_overflow_count: u32,
    _pad_counters: vec2<u32>,
    final_blend_args: EdgeIndirectArgsRO,
    skybox_edge_args: EdgeIndirectArgsRO,
    {% for entry in bucket_entries %}
    {{ entry.args_field() }}_edge: EdgeIndirectArgsRO,
    {% endfor %}
    data: array<u32>,
};

@group(0) @binding(0) var<storage, read> edge_buffers: EdgeBuffersRO;

struct EdgeBufferLayoutRO {
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

@group(0) @binding(1) var<uniform> edge_layout: EdgeBufferLayoutRO;

// Opaque target texture — final blend writes resolved edge pixels here.
//
// Format templated to match the runtime render-texture format ({{ color_format }}).
@group(0) @binding(2) var opaque_tex: texture_storage_2d<{{ color_format }}, write>;
