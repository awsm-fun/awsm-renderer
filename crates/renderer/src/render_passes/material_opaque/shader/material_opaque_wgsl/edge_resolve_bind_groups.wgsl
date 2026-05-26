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
// Group(3) extension: edge-resolve emission buffers + layout uniform.
//
// `shared_wgsl/shadow/bind_groups.wgsl` (included above by way of
// bind_groups.wgsl) declares bindings 0..=9 at this group; we append
// bindings 10 and 11 here to carry the edge buffer (read-write
// storage) and the edge-layout uniform.

struct EdgeIndirectArgs {
    workgroup_count_x: u32,
    workgroup_count_y: u32,
    workgroup_count_z: u32,
    _pad: u32,
};

struct EdgeBuffersReadOnly {
    edge_count: u32,
    edge_overflow_count: u32,
    _pad_counters: vec2<u32>,
    final_blend_args: EdgeIndirectArgs,
    skybox_edge_args: EdgeIndirectArgs,
    {% for entry in bucket_entries %}
    {{ entry.args_field() }}_edge: EdgeIndirectArgs,
    {% endfor %}
    data: array<u32>,
};

// Edge-resolve writes to the accumulator slot region (per-thread; each
// (edge_pixel_id, slot_index) is owned exclusively) — so the binding
// is read_write. Atomic counters are unused on this side.
@group({{ shadow_group_index }}) @binding(10) var<storage, read_write> edge_buffers: EdgeBuffersReadOnly;

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
