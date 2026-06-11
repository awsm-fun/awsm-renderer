// Bind groups for the MASKED (alpha-tested) geometry raster variant.
//
// Groups 1 (transforms), 2 (uniform meta) and 3 (animation) are byte-for-byte
// the plain geometry pass's NON-instanced / uniform-meta bindings — the masked
// variant reuses the shared vertex shader verbatim, so the vertex-stage layout
// must match. Group 0 is AUGMENTED: it keeps the camera + frame_globals
// uniforms the vertex reads, then appends the fragment-only bindings the
// alpha-test needs (materials, per-mesh material meta, the merged geometry pool,
// texture transforms, and the texture pool). Staying on group 0 keeps the
// variant within the maxBindGroups=4 ceiling.
//
// CameraRaw / FrameGlobalsRaw / GeometryMeshMeta come from the vertex includes;
// MaterialMeshMeta / TextureInfo / TextureTransform from the fragment includes.
// WGSL resolves module-scope identifiers order-independently, so referencing
// them here (before those includes in the concatenated source) is fine — the
// plain geometry pass relies on the same property.

// === Group 0: camera/frame_globals (vertex+fragment) + masked fragment data ===
@group(0) @binding(0) var<uniform> camera_raw: CameraRaw;
@group(0) @binding(1) var<uniform> frame_globals_raw: FrameGlobalsRaw;
// Renderer-wide per-material data pool (raw u32 words). Read at
// `material_mesh_meta.material_offset` via the `material_load_*` helpers.
@group(0) @binding(2) var<storage, read> materials: array<u32>;
// Per-mesh material meta (256-byte aligned slots), indexed by the
// `material_mesh_meta_offset` flat varying the vertex forwards.
@group(0) @binding(3) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;
// The merged geometry pool — same buffer the opaque compute aliases as
// `visibility_data`. Holds the per-mesh attribute-index + attribute-data
// sections this fragment reads (UVs) at the offsets in MaterialMeshMeta.
@group(0) @binding(4) var<storage, read> visibility_data: array<f32>;
// Per-material UV transforms (KHR_texture_transform). Referenced by
// `texture_transform_uvs` in textures.wgsl.
@group(0) @binding(5) var<storage, read> texture_transforms: array<TextureTransform>;
{% for i in 0..texture_pool_arrays_len %}
@group(0) @binding({{ 6 + i }}u) var pool_tex_{{ i }}: texture_2d_array<f32>;
{% endfor %}
{% for i in 0..texture_pool_samplers_len %}
@group(0) @binding({{ 6 + texture_pool_arrays_len + i }}u) var pool_sampler_{{ i }}: sampler;
{% endfor %}

// === Group 1: transforms (vertex) — mirrors geometry bind_groups ===
struct TransformPacked {
    model_world: mat4x4<f32>,
    normal_world: mat3x3<f32>,
};
@group(1) @binding(0) var<storage, read> transforms: array<TransformPacked>;

// === Group 2: per-mesh geometry meta (uniform + dynamic offset, vertex) ===
@group(2) @binding(0) var<uniform> geometry_mesh_meta: GeometryMeshMeta;

// === Group 3: morph + skin animation (vertex) — mirrors geometry bind_groups ===
@group(3) @binding(0) var<storage, read> geometry_morph_weights: array<f32>;
@group(3) @binding(1) var<storage, read> geometry_morph_values: array<f32>;
@group(3) @binding(2) var<storage, read> skin_joint_matrices: array<mat4x4<f32>>;
@group(3) @binding(3) var<storage, read> skin_joint_index_weights: array<f32>;
