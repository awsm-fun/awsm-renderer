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
// Group(3) extension: edge-resolve data buffer + layout uniform.
//
// `shared_wgsl/shadow/bind_groups.wgsl` (included above by way of
// bind_groups.wgsl) declares bindings 0..=9 at this group; we append
// bindings 10/11 here to carry the data buffer (read-write storage)
// and the edge-layout uniform.
//
// The args_buffer is NOT bound here — its atomic counters are
// mirrored into `edge_data`'s header (offsets supplied via
// `edge_layout`). This keeps the compute stage's storage-buffer count
// at 10 (the WebGPU baseline) instead of bumping it to 11.

// data_buffer: small counter-mirror header + edge_to_xy + edge_slot_map
// + accumulator + sample lists, indexed via the EdgeBufferLayout
// uniform's u32-stride offsets.
@group({{ shadow_group_index }}) @binding(10) var<storage, read_write> edge_data: array<u32>;

struct EdgeBufferLayout {
    max_edge_budget: u32,
    // u32-stride indices into `edge_data` of the atomic-counter
    // mirrors classify writes during edge emission.
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

@group({{ shadow_group_index }}) @binding(11) var<uniform> edge_layout: EdgeBufferLayout;
