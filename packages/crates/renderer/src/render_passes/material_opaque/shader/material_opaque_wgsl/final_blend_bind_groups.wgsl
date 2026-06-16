// Bind-group declarations for the final_blend compositor.
//
// Only needs: the edge buffer (read-only counters + per-edge arrays +
// accumulator) and the opaque storage texture as the write target.

// data_buffer (read-only): small counter-mirror header + edge_to_xy +
// edge_slot_map + accumulator + sample lists. final_blend reads
// edge_count from the header, plus edge_to_xy + accumulator.
@group(0) @binding(0) var<storage, read> edge_data: array<u32>;

struct EdgeBufferLayoutRO {
    max_edge_budget: u32,
    edge_count_index: u32,
    per_shader_count_base: u32,
    skybox_count_index: u32,
    edge_to_xy_base: u32,
    edge_slot_map_base: u32,
    accumulator_base: u32,
    // Base of bucket 0's sample list; bucket `i` at
    // `per_shader_sample_list_base + i * sample_entries_per_bucket` (§4c).
    per_shader_sample_list_base: u32,
    skybox_sample_list_base: u32,
    sample_entries_per_bucket: u32,
};

@group(0) @binding(1) var<uniform> edge_layout: EdgeBufferLayoutRO;

// Opaque target texture — final blend writes resolved edge pixels here.
//
// Format templated to match the runtime render-texture format ({{ color_format }}).
@group(0) @binding(2) var opaque_tex: texture_storage_2d<{{ color_format }}, write>;
