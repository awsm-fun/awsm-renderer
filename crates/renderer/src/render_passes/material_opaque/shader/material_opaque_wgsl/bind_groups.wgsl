// `instance_attrs` (binding 23) uses `InstanceAttr`; declare the struct here
// so the binding's type is in scope at parse time.
{% include "shared_wgsl/instance_attrs.wgsl" %}

{% if multisampled_geometry %}
    @group(0) @binding(0) var visibility_data_tex: texture_multisampled_2d<u32>;
    // Barycentric tex packs: RG = bary.xy as u16 fixed-point, BA = instance_id (split u32).
    @group(0) @binding(1) var barycentric_tex: texture_multisampled_2d<u32>;
    @group(0) @binding(2) var depth_tex: texture_depth_multisampled_2d;
    @group(0) @binding(3) var normal_tangent_tex: texture_multisampled_2d<f32>;
    @group(0) @binding(4) var barycentric_derivatives_tex: texture_multisampled_2d<f32>;
{% else %}
    @group(0) @binding(0) var visibility_data_tex: texture_2d<u32>;
    @group(0) @binding(1) var barycentric_tex: texture_2d<u32>;
    @group(0) @binding(2) var depth_tex: texture_depth_2d;
    @group(0) @binding(3) var normal_tangent_tex: texture_2d<f32>;
    @group(0) @binding(4) var barycentric_derivatives_tex: texture_2d<f32>;
{% endif %}
@group(0) @binding(5) var<storage, read> visibility_data: array<f32>;
@group(0) @binding(6) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;
@group(0) @binding(7) var<storage, read> materials: array<u32>;
@group(0) @binding(8) var<storage, read> attribute_indices: array<u32>;
@group(0) @binding(9) var<storage, read> attribute_data: array<f32>;
// Packed transform (Option E): each entry is model (mat4x4) + normal
// matrix (mat3x3 with vec3-column padding). The shader reads both
// from the same array; `Transforms::BYTE_SIZE` = 112 = stride.
struct TransformPacked {
    model_world: mat4x4<f32>,
    normal_world: mat3x3<f32>,
};
@group(0) @binding(10) var<storage, read> transforms: array<TransformPacked>;
@group(0) @binding(11) var<storage, read> texture_transforms: array<TextureTransform>;
@group(0) @binding(12) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(13) var skybox_tex: texture_cube<f32>;
@group(0) @binding(14) var skybox_sampler: sampler;
@group(0) @binding(15) var ibl_filtered_env_tex: texture_cube<f32>;
@group(0) @binding(16) var ibl_filtered_env_sampler: sampler;
@group(0) @binding(17) var ibl_irradiance_tex: texture_cube<f32>;
@group(0) @binding(18) var ibl_irradiance_sampler: sampler;
@group(0) @binding(19) var brdf_lut_tex: texture_2d<f32>;
@group(0) @binding(20) var brdf_lut_sampler: sampler;
@group(0) @binding(21) var opaque_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(22) var<storage, read> instance_attrs: array<InstanceAttr>;

@group(1) @binding(0) var<uniform> lights_info: LightsInfoPacked;
// `lights` is a uniform array (Option F follow-up to Cluster 2.1.c).
// Uniform memory is constant-cached for the lockstep per-pixel walk;
// the hard cap (64 KB / 64 B) is `MAX_PUNCTUAL_LIGHTS` = 1024 lights.
// `MAX_PUNCTUAL_LIGHTS` is the Rust-side constant; the WGSL array
// length must match it exactly for binding-size validation.
@group(1) @binding(1) var<uniform> lights: array<LightPacked, 1024>;
// Per-mesh light-list path (Cluster 2.1.c). Slice metadata
// (`light_slice_offset` + `light_slice_count`) now lives inside
// `MaterialMeshMeta` so each pixel reads it for free as part of the
// already-required `material_mesh_metas[meta_index]` load — one
// storage-buffer slot saved. The indices buffer stays separate
// because its size is variable (sum of all slice counts).
@group(1) @binding(2) var<storage, read> mesh_light_indices: array<u32>;

{% for i in 0..texture_pool_arrays_len %}
    @group(2) @binding({{ i }}u) var pool_tex_{{ i }}: texture_2d_array<f32>;
{% endfor %}
{% for i in 0..texture_pool_samplers_len %}
    @group(2) @binding({{ texture_pool_arrays_len + i }}u) var pool_sampler_{{ i }}: sampler;
{% endfor %}

// === Shadow bind group (group 3) ===
// Layout fixed across phases — actual sampling helpers added when the
// real shadow generation lands. Phase 0: declarations only.
{% include "shared_wgsl/shadow/bind_groups.wgsl" %}
