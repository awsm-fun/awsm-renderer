// Bind-group declarations for the per-shader-id edge_resolve compute
// shader. Shares the texture/lights/shadows binding shape with the
// primary opaque pipeline so the same texture-pool / lights / shadows
// bind groups can be reused at dispatch time. Adds the edge-buffer
// + layout bindings at group(4) — distinct group index from the
// classify pass's binding so concurrent dispatches don't collide.

// ─────────────────────────────────────────────────────────────────
// Group(0): main textures + buffers (same as primary opaque pass).
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
// Group(4): edge-resolve emission buffers + layout uniform.

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
@group(4) @binding(0) var<storage, read_write> edge_buffers: EdgeBuffersReadOnly;

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

@group(4) @binding(1) var<uniform> edge_layout: EdgeBufferLayout;
