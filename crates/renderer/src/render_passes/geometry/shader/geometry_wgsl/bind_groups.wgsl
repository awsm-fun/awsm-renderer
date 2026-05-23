@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
// Packed transforms (model + normal). Geometry pass only uses
// `.model_world` — see `get_model_transform` in
// `shared_wgsl/vertex/transform.wgsl`. The normal matrix slot is
// adjacent in memory but unread here.
struct TransformPacked {
    model_world: mat4x4<f32>,
    normal_world: mat3x3<f32>,
};
@group(1) @binding(0) var<storage, read> transforms: array<TransformPacked>;
{% if meta_storage_array %}
// Non-instanced meshes (under the `indirect-first-instance` WebGPU
// feature) read meta from a storage-buffer array indexed by
// `@builtin(instance_index)`. The compaction shader writes
// `IndirectDrawArgs.first_instance = mesh_slot` so each mesh's
// drawIndirect picks the correct slot — one shared @group(2)
// binding services every draw, no per-draw `setBindGroup` cost.
// `var<private>` mirrors the previous `geometry_mesh_meta` symbol
// so the shared helpers in
// `shared_wgsl/vertex/{apply_vertex,morph,skin}.wgsl` keep
// compiling without parameter threading.
@group(2) @binding(0) var<storage, read> geometry_mesh_metas: array<GeometryMeshMeta>;
var<private> geometry_mesh_meta: GeometryMeshMeta;
{% else %}
// Uniform-with-dynamic-offset binding. Used by:
//   - every instanced draw (the `instance_index` range across one
//     drawIndirect's instances would collide with neighbouring
//     meshes' meta slots in a shared storage array).
//   - the portable non-instanced path (`indirect_first_instance`
//     off), where the CPU calls
//     `setBindGroup(2, group, &[meta_offset])` before each
//     drawIndirect / drawIndexed call. The `first_instance` slot
//     in indirect args stays at 0; the bind-group dynamic offset
//     carries the per-mesh slot identity instead.
@group(2) @binding(0) var<uniform> geometry_mesh_meta: GeometryMeshMeta;
{% endif %}
@group(3) @binding(0) var<storage, read> geometry_morph_weights: array<f32>;
@group(3) @binding(1) var<storage, read> geometry_morph_values: array<f32>;
@group(3) @binding(2) var<storage, read> skin_joint_matrices: array<mat4x4<f32>>;
// Joint buffer - indexed per original vertex (matches morph pattern)
// We interleave indices with weights and get our index back losslessly via bitcast
// Layout: vertex 0: [joints_0, joints_1, ...], vertex 1: [joints_0, joints_1, ...], etc.
@group(3) @binding(3) var<storage, read> skin_joint_index_weights: array<f32>;
