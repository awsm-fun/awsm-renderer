// `instance_attrs` (binding 9) uses `InstanceAttr`; declare the struct here
// so the binding's type is in scope at parse time.
{% include "shared_wgsl/instance_attrs.wgsl" %}

@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
// Packed transforms (model + normal). Transparent's vertex shader
// only reads `.model_world`; the fragment shader's `get_transforms`
// helper reads both.
struct TransformPacked {
    model_world: mat4x4<f32>,
    normal_world: mat3x3<f32>,
};
@group(0) @binding(1) var<storage, read> transforms: array<TransformPacked>;
@group(0) @binding(2) var<storage, read> materials: array<u32>;
@group(0) @binding(3) var<storage, read> geometry_morph_weights: array<f32>;
@group(0) @binding(4) var<storage, read> geometry_morph_values: array<f32>;
@group(0) @binding(5) var<storage, read> skin_joint_matrices: array<mat4x4<f32>>;
// Joint buffer - indexed per original vertex (matches morph pattern)
// We interleave indices with weights and get our index back losslessly via bitcast
// Layout: vertex 0: [joints_0, joints_1, ...], vertex 1: [joints_0, joints_1, ...], etc.
@group(0) @binding(6) var<storage, read> skin_joint_index_weights: array<f32>;
@group(0) @binding(7) var<storage, read> texture_transforms: array<TextureTransform>;
@group(0) @binding(8) var opaque_tex: texture_2d<f32>;
@group(0) @binding(9) var<storage, read> instance_attrs: array<InstanceAttr>;
// ─── Lights folded into group 0 in 16.B (was @group(1)) ────────────
@group(0) @binding(10) var ibl_filtered_env_tex: texture_cube<f32>;
@group(0) @binding(11) var ibl_filtered_env_sampler: sampler;
@group(0) @binding(12) var ibl_irradiance_tex: texture_cube<f32>;
@group(0) @binding(13) var ibl_irradiance_sampler: sampler;
@group(0) @binding(14) var brdf_lut_tex: texture_2d<f32>;
@group(0) @binding(15) var brdf_lut_sampler: sampler;
@group(0) @binding(16) var<uniform> lights_info: LightsInfoPacked;
// Lights are uniform (Option F follow-up to Cluster 2.1.c). Same
// fixed-size 1024-entry array as the opaque pass.
@group(0) @binding(17) var<uniform> lights: array<LightPacked, 1024>;

// ─── Shadow bind group (group 1 in 16.B) ───────────────────────────
// Includes the shared shadow bindings (atlas + cube + EVSM + globals
// + descriptor uniform array) plus the helper functions used by
// `apply_lighting`-style sampling. The `shadow_group_index` template
// var is what `bind_groups.wgsl` keys on for `@group(N)` decls.
{% include "shared_wgsl/shadow/bind_groups.wgsl" %}


{% for i in 0..texture_pool_arrays_len %}
    @group(2) @binding({{ i }}u) var pool_tex_{{ i }}: texture_2d_array<f32>;
{% endfor %}
{% for i in 0..texture_pool_samplers_len %}
    @group(2) @binding({{ texture_pool_arrays_len + i }}u) var pool_sampler_{{ i }}: sampler;
{% endfor %}

@group(3) @binding(0) var<uniform> geometry_mesh_meta: GeometryMeshMeta;
@group(3) @binding(1) var<uniform> material_mesh_meta: MaterialMeshMeta;
