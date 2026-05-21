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
{% if instancing_transforms %}
// Instanced meshes (curve-instances, prefab-instances) keep the
// legacy uniform-with-dynamic-offset binding. The `instance_index`
// range across instances of one drawIndirect would otherwise
// collide with neighboring meshes' meta slots if it indexed a
// shared storage array — moving them off this path requires their
// per-instance data to live in a parallel attribute array. For now
// they stay on the dynamic-offset path and the CPU
// `draw_indexed_with_instance_count` recording.
@group(2) @binding(0) var<uniform> geometry_mesh_meta: GeometryMeshMeta;
{% else %}
// Non-instanced meshes read meta from a storage-buffer array
// indexed by `@builtin(instance_index)`. The
// CPU sets `first_instance = mesh_meta_idx` so each mesh's draw
// picks the correct slot, allowing one shared @group(2) binding
// across all draws + an indirect-draw path under
// `features.gpu_culling`. `var<private>` mirrors the previous
// `geometry_mesh_meta` symbol so the shared helpers in
// `shared_wgsl/vertex/{apply_vertex,morph,skin}.wgsl` keep
// compiling without parameter threading.
@group(2) @binding(0) var<storage, read> geometry_mesh_metas: array<GeometryMeshMeta>;
var<private> geometry_mesh_meta: GeometryMeshMeta;
{% endif %}
@group(3) @binding(0) var<storage, read> geometry_morph_weights: array<f32>;
@group(3) @binding(1) var<storage, read> geometry_morph_values: array<f32>;
@group(3) @binding(2) var<storage, read> skin_joint_matrices: array<mat4x4<f32>>;
// Joint buffer - indexed per original vertex (matches morph pattern)
// We interleave indices with weights and get our index back losslessly via bitcast
// Layout: vertex 0: [joints_0, joints_1, ...], vertex 1: [joints_0, joints_1, ...], etc.
@group(3) @binding(3) var<storage, read> skin_joint_index_weights: array<f32>;
