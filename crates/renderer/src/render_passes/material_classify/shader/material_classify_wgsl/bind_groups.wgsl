// Material classify pass bind groups.
//
// One bind group:
//   0: visibility_data_tex — sampled to recover per-pixel material id.
//   1: material_mesh_metas — storage[RO] mesh-meta table for the
//      `material_meta_offset → material_offset` step.
//   2: materials_data     — storage[RO] material payload; the first
//      `u32` of each entry is the `shader_id`.
//   3: classify_output    — storage[RW] (atomic) buckets + indirect
//      args. Layout matches the
//      `ClassifyBuffers` Rust-side header verbatim and is generated
//      per-registration by walking `bucket_entries` (first-party +
//      every registered dynamic material).

/*************** START material_mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END material_mesh_meta.wgsl ******************/

// `join32` and `U32_MAX` come from `shared_wgsl/math.wgsl`, included
// once by `compute.wgsl` (concatenated after this file).

{% if multisampled_geometry %}
@group(0) @binding(0) var visibility_data_tex: texture_multisampled_2d<u32>;
{% else %}
@group(0) @binding(0) var visibility_data_tex: texture_2d<u32>;
{% endif %}

@group(0) @binding(1) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;

@group(0) @binding(2) var<storage, read> materials_data: array<u32>;

struct ClassifyIndirectArgs {
    workgroup_count_x: atomic<u32>,
    workgroup_count_y: u32,
    workgroup_count_z: u32,
    _pad: u32,
};

// Storage-buffer layout — must stay in lockstep with the byte writer
// in `material_classify::buffers::write_header`. The N indirect args
// slots are at offsets `i * 16`; `dispatchWorkgroupsIndirect` reads
// each as `(x, y, z)` from the bound buffer at that offset.
//
// Generated per-registration: one `args_<name>` field per bucket,
// then one `<name>_offset` field per bucket, then the shared
// `bucket_capacity`, then `pad_words` words of alignment padding so
// the trailing `tiles` array (vec2<u32>, 8 B stride) starts on a
// 16-byte boundary. The host's `header_bytes(bucket_count)` matches.
struct ClassifyOutput {
{% for entry in bucket_entries %}
    {{ entry.args_field() }}: ClassifyIndirectArgs,
{% endfor %}
{% for entry in bucket_entries %}
    {{ entry.offset_field() }}: u32,
{% endfor %}
    bucket_capacity: u32,
{% for pad in pad_words_iter %}
    _pad_align_{{ pad }}: u32,
{% endfor %}
    tiles: array<vec2<u32>>,
};

@group(0) @binding(3) var<storage, read_write> classify_output: ClassifyOutput;

{% if emit_edge_data %}
// ─────────────────────────────────────────────────────────────────
// Priority-3 MSAA edge-resolve emission buffers.
//
// Layout matches `render_passes::material_opaque::edge_buffers`:
//   - edge_count: atomic<u32> — bytes [0, 4)
//   - edge_overflow_count: atomic<u32> — bytes [4, 8)
//   - 8 bytes alignment pad
//   - final_blend_args: ClassifyIndirectArgs — bytes [16, 32)
//   - skybox_edge_args: ClassifyIndirectArgs — bytes [32, 48)
//   - per_shader_id_args: array<ClassifyIndirectArgs, bucket_count>
//   - edge_to_xy: array<u32, MAX_EDGE_BUDGET>          (packed x:16, y:16)
//   - edge_slot_map: array<u32, MAX_EDGE_BUDGET>       (4 shader_ids × 8 bits)
//   - accumulator: array<vec4<f32>, MAX_EDGE_BUDGET × 4>
//   - per-bucket sample entries: array<u32>            (packed id:24, mask:8)
//
// The classify shader writes the counters + edge_to_xy + edge_slot_map
// + per-bucket sample lists. The per-shader edge_resolve pipelines
// read the sample lists and write the accumulator slots; final_blend
// reads the accumulator + edge_to_xy and writes opaque_tex.
struct EdgeIndirectArgs {
    workgroup_count_x: atomic<u32>,
    workgroup_count_y: u32,
    workgroup_count_z: u32,
    _pad: u32,
};

// The atomic counters live at the head of the buffer; the indirect
// args follow at fixed offsets known to the host. The per-shader-id
// sample-list region is variable-sized so we declare it via a
// trailing runtime-sized array at the end (the host computes its
// offset per-bucket and indexes via that base).
struct EdgeBuffers {
    edge_count: atomic<u32>,
    edge_overflow_count: atomic<u32>,
    _pad_counters: vec2<u32>,
    final_blend_args: EdgeIndirectArgs,
    skybox_edge_args: EdgeIndirectArgs,
    {% for entry in bucket_entries %}
    {{ entry.args_field() }}_edge: EdgeIndirectArgs,
    {% endfor %}
    // Trailing variable-size region. The host knows the offsets of
    // edge_to_xy / edge_slot_map / accumulator / sample lists relative
    // to the start of `data`, indexed as flat u32 storage.
    data: array<u32>,
};

@group(0) @binding(4) var<storage, read_write> edge_buffers: EdgeBuffers;

// Host-uploaded constants for offsetting into `edge_buffers.data`. All
// values in u32-stride units (storage-buffer arrays index by element
// size, so 4-byte u32s naturally line up).
struct EdgeBufferLayout {
    max_edge_budget: u32,
    edge_to_xy_base: u32,
    edge_slot_map_base: u32,
    accumulator_base: u32,
    {% for entry in bucket_entries %}
    {{ entry.args_field() }}_sample_list_base: u32,
    {% endfor %}
    sample_entries_per_bucket: u32,
};

@group(0) @binding(5) var<uniform> edge_layout: EdgeBufferLayout;
{% endif %}
