// `instance_attrs` (binding 20) uses `InstanceAttr`; declare the struct here
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
// `visibility_data` is a view over the merged geometry pool — per-mesh
// sections (visibility, attribute_indices, attribute_data)
// are addressed at the sub-offsets carried by MaterialMeshMeta. The
// declared element type stays `f32` because position/normal reads stay
// natural; u32 reads (attribute indices) come through `bitcast<u32>(…)`.
@group(0) @binding(5) var<storage, read> visibility_data: array<f32>;
@group(0) @binding(6) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;
@group(0) @binding(7) var<storage, read> materials: array<u32>;
// Packed transform (Option E): each entry is model (mat4x4) + normal
// matrix (mat3x3 with vec3-column padding). The shader reads both
// from the same array; `Transforms::BYTE_SIZE` = 112 = stride.
struct TransformPacked {
    model_world: mat4x4<f32>,
    normal_world: mat3x3<f32>,
};
@group(0) @binding(8) var<storage, read> transforms: array<TransformPacked>;
@group(0) @binding(9) var<storage, read> texture_transforms: array<TextureTransform>;
@group(0) @binding(10) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(11) var skybox_tex: texture_cube<f32>;
@group(0) @binding(12) var skybox_sampler: sampler;
@group(0) @binding(13) var ibl_filtered_env_tex: texture_cube<f32>;
@group(0) @binding(14) var ibl_filtered_env_sampler: sampler;
@group(0) @binding(15) var ibl_irradiance_tex: texture_cube<f32>;
@group(0) @binding(16) var ibl_irradiance_sampler: sampler;
@group(0) @binding(17) var brdf_lut_tex: texture_2d<f32>;
@group(0) @binding(18) var brdf_lut_sampler: sampler;
@group(0) @binding(19) var opaque_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(20) var<storage, read> instance_attrs: array<InstanceAttr>;

// Material classify output (read-only here — the read-write atomic
// view is bound on the classify pass). Layout matches
// `ClassifyOutput` in `material_classify_wgsl/bind_groups.wgsl`; the
// indirect-args header is consumed by `dispatchWorkgroupsIndirect`
// host-side. The shader only reads `*_offset` + `tiles[…]` to map
// `workgroup_id.x` back to a tile's `(x, y)` coords.
// Read-only view of the classify-pass output. Layout MUST match the
// classify-pass writer's `ClassifyOutput` struct byte-for-byte —
// both are templated from the same `bucket_entries`.
struct ClassifyBuckets {
{% for entry in bucket_entries %}
    {{ entry.args_field() }}: vec4<u32>,
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
@group(0) @binding(21) var<storage, read> classify_buckets: ClassifyBuckets;

// Renderer-wide per-frame uniform — see `shared_wgsl/frame_globals.wgsl`
// for layout. Rides alongside the camera uniform; one upload per frame.
@group(0) @binding(22) var<uniform> frame_globals_raw: FrameGlobalsRaw;

// Renderer-wide variable-length per-material data pool. Backs
// `BufferSlot` declarations on registered dynamic materials. See
// `shared_wgsl/extras.wgsl` for the load helpers and
// `crates/renderer/src/dynamic_materials/extras_pool.rs` for the
// host-side allocator.
@group(0) @binding(23) var<storage, read> extras_pool: array<u32>;

@group(1) @binding(0) var<uniform> lights_info: LightsInfoPacked;
// `lights` is a uniform array.
// Uniform memory is constant-cached for the lockstep per-pixel walk;
// the hard cap (64 KB / 64 B) is `MAX_PUNCTUAL_LIGHTS` = 1024 lights.
// `MAX_PUNCTUAL_LIGHTS` is the Rust-side constant; the WGSL array
// length must match it exactly for binding-size validation.
@group(1) @binding(1) var<uniform> lights: array<LightPacked, 1024>;
// `lights_storage`: merged per-mesh + per-froxel u32 array.
// Head region `[0..cull_params.mesh_indices_capacity_u32)` carries the
// CPU-written per-mesh light indices (consumed via the per-mesh slice
// fields in `MaterialMeshMeta`). Tail region carries the GPU cull
// pass's per-froxel slices (consumed via the per-pixel
// `apply_lighting_per_froxel*` helpers when the oversized sentinel
// `light_slice_count == 0xFFFFFFFFu` fires).
//
// Merging the two regions onto one binding keeps the opaque compute
// stage under WebGPU's `maxStorageBuffersPerShaderStage` ceiling.
@group(1) @binding(2) var<storage, read> lights_storage: array<u32>;
// `cull_params`: per-frame uniform written by the cull pass. The
// per-pixel froxel index calc reads `tiles_x/y`, `viewport_w/h`,
// `z_near/z_far`, `log_far_over_near`, and `mesh_indices_capacity_u32`
// (the head→tail boundary in `lights_storage`).
//
// The struct decl is duplicated from the cull pass's
// `light_culling_wgsl/bind_groups.wgsl`; both must stay byte-aligned.
struct CullParams {
    tiles_x: u32,
    tiles_y: u32,
    viewport_w: u32,
    viewport_h: u32,
    mesh_indices_capacity_u32: u32,
    max_per_froxel_capacity: u32,
    _pad0: u32,
    z_near: f32,
    z_far: f32,
    log_far_over_near: f32,
    _pad1: f32, _pad2: f32,
};
@group(1) @binding(3) var<uniform> cull_params: CullParams;

{% for i in 0..texture_pool_arrays_len %}
    @group(2) @binding({{ i }}u) var pool_tex_{{ i }}: texture_2d_array<f32>;
{% endfor %}
{% for i in 0..texture_pool_samplers_len %}
    @group(2) @binding({{ texture_pool_arrays_len + i }}u) var pool_sampler_{{ i }}: sampler;
{% endfor %}

// === Shadow bind group (group 3) ===
{% include "shared_wgsl/shadow/bind_groups.wgsl" %}
