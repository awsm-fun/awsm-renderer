// Bind-group declarations for the per-shader-id edge_resolve compute
// shader. Shares the texture/lights binding shape with the primary
// opaque pipeline so the same texture-pool / lights bind groups can
// be reused at dispatch time. The edge-buffer + layout bindings used
// to live in a separate group(4); they are now appended to the
// shadow bind group at group(3) (bindings 10 and 11) so the whole
// layout fits in 4 bind groups — required to activate on devices
// with `maxBindGroups = 4` (macOS Metal in particular).

// ─────────────────────────────────────────────────────────────────
// Group(0..3): main textures + buffers / lights / texture-pool /
// shadows (same as primary opaque pass). The shadow group below is
// extended at the end with the edge-buffer + edge-layout bindings.
//
// For brevity, the edge_resolve pipelines reuse the exact same group(0)
// bindings as compute.wgsl's primary path — see
// material_opaque_wgsl/bind_groups.wgsl for the full declaration.
// Includes the visibility/barycentric/depth/normal_tangent multisampled
// textures, the geometry pool / mesh meta / materials / transforms /
// camera / skybox / IBL / brdf / opaque storage tex / instance attrs
// / classify buckets / frame globals / extras pool.

{% include "material_opaque_wgsl/bind_groups.wgsl" %}

// ─────────────────────────────────────────────────────────────────
// Group(3) extension: edge-resolve data buffer + layout uniform +
// args buffer (read-only).
//
// `shared_wgsl/shadow/bind_groups.wgsl` (included above by way of
// bind_groups.wgsl) declares bindings 0..=9 at this group; we append
// bindings 10/11/12 here to carry the data buffer (read-write
// storage), the edge-layout uniform, and the args buffer (read-only
// storage — also the indirect-dispatch source, but Indirect +
// Storage(read) on the same buffer is allowed by WebGPU since neither
// usage is writable).
//
// The args/data buffer split is what unblocks Stage 3 — a single
// buffer that's simultaneously bound as Storage(read-write) AND used
// as Indirect inside one compute pass is rejected; splitting the
// indirect args + counters into a separate buffer from the
// storage-writable accumulator sidesteps it entirely.

struct EdgeIndirectArgsRO {
    workgroup_count_x: u32,
    workgroup_count_y: u32,
    workgroup_count_z: u32,
    _pad: u32,
};

// args_buffer-shaped struct (read-only — counters + indirect-args only).
struct EdgeArgsBufferRO {
    edge_count: u32,
    edge_overflow_count: u32,
    _pad_counters: vec2<u32>,
    final_blend_args: EdgeIndirectArgsRO,
    skybox_edge_args: EdgeIndirectArgsRO,
    {% for entry in bucket_entries %}
    {{ entry.args_field() }}_edge: EdgeIndirectArgsRO,
    {% endfor %}
};

// data_buffer: edge_to_xy + edge_slot_map + accumulator + sample lists,
// indexed via the EdgeBufferLayout uniform's u32-stride offsets.
@group({{ shadow_group_index }}) @binding(10) var<storage, read_write> edge_data: array<u32>;

struct EdgeBufferLayout {
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

@group({{ shadow_group_index }}) @binding(11) var<uniform> edge_layout: EdgeBufferLayout;

@group({{ shadow_group_index }}) @binding(12) var<storage, read> edge_args: EdgeArgsBufferRO;
