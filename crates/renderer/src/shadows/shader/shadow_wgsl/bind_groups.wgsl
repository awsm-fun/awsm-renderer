// Shadow generation pass bindings.
//
// The shadow VS reuses the geometry pass's transform / meta / animation
// bind groups verbatim — the layouts are bound at the same slots (1, 2,
// 3) so the same per-mesh metadata, model transforms, morph weights /
// values, and skin joint matrices are visible.
//
// Group 0 is a per-view uniform written from `Shadows::write_gpu` with
// the current shadow view's light-space matrix.

struct ShadowView {
    view_projection: mat4x4<f32>,
    // (depth_bias, normal_bias, 0, 0) — bias is applied at sample time;
    // these are passed along for inspector visibility.
    bias: vec4<f32>,
};

@group(0) @binding(0) var<uniform> shadow_view: ShadowView;
// Packed transforms (model + normal). Shadow pass only needs
// `.model_world` — same `get_model_transform` helper as the geometry
// pass.
struct TransformPacked {
    model_world: mat4x4<f32>,
    normal_world: mat3x3<f32>,
};
@group(1) @binding(0) var<storage, read> transforms: array<TransformPacked>;
@group(2) @binding(0) var<uniform> geometry_mesh_meta: GeometryMeshMeta;
@group(3) @binding(0) var<storage, read> geometry_morph_weights: array<f32>;
@group(3) @binding(1) var<storage, read> geometry_morph_values: array<f32>;
@group(3) @binding(2) var<storage, read> skin_joint_matrices: array<mat4x4<f32>>;
@group(3) @binding(3) var<storage, read> skin_joint_index_weights: array<f32>;
