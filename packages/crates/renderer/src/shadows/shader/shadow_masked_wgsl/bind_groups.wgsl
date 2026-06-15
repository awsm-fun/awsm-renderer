// Bind groups for the MASKED (alpha-tested) shadow generation pass.
//
// Mirrors the masked GEOMETRY variant's group-0 augmentation, but for the
// shadow pass: group 0 keeps the per-view `shadow_view` uniform the vertex
// needs, then appends the fragment-only data the cutout alpha-test reads
// (materials, per-mesh material meta, the merged geometry pool, texture
// transforms, and the texture pool). Groups 1 (transforms), 2 (meta, forked by
// instancing) and 3 (animation) are the geometry pass's vertex bindings,
// verbatim. Staying on group 0 keeps the variant within maxBindGroups=4.
//
// MaterialMeshMeta / TextureInfo / TextureTransform come from the fragment
// includes (shared_wgsl/masked_alpha.wgsl); GeometryMeshMeta from the vertex
// include. WGSL resolves module-scope identifiers order-independently, so
// referencing them here is fine — the masked geometry pass relies on the same.

// `FrameGlobalsRaw` + `frame_globals_from_raw` for the `frame_globals_raw`
// uniform below (an animated cutout's alpha-only window reads `time`).
{% include "shared_wgsl/frame_globals.wgsl" %}

struct ShadowView {
    view_projection: mat4x4<f32>,
    // (depth_bias, normal_bias, 0, 0) — unused by the cutout fragment; carried
    // for parity with the plain shadow_view uniform layout.
    bias: vec4<f32>,
};
@group(0) @binding(0) var<uniform> shadow_view: ShadowView;
// Renderer-wide per-material data pool (raw u32 words). Read at
// `material_mesh_meta.material_offset` via the `material_load_*` helpers.
@group(0) @binding(1) var<storage, read> materials: array<u32>;
// Per-mesh material meta (256-byte aligned slots), indexed by the
// `material_mesh_meta_offset` flat varying the vertex forwards.
@group(0) @binding(2) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;
// The merged geometry pool — same buffer the opaque compute aliases as
// `visibility_data`. Holds the per-mesh attribute-index + attribute-data
// sections this fragment reads (UVs) at the offsets in MaterialMeshMeta.
@group(0) @binding(3) var<storage, read> visibility_data: array<f32>;
// Per-material UV transforms (KHR_texture_transform). Referenced by
// `texture_transform_uvs` in textures.wgsl.
@group(0) @binding(4) var<storage, read> texture_transforms: array<TextureTransform>;
// Per-frame uniform (`time`, etc.). The plain shadow pass doesn't need it, but
// the cutout alpha-test does: a custom alpha-only window may be a function of
// `time` (an animated / procedural cutout), and the SAME author fragment runs in
// both the geometry-masked AND shadow-masked passes — so the shadow variant must
// expose `frame_globals_raw` too, or referencing it there fails to compile.
// Fragment-only (the shadow vertex uses `shadow_view`, not time).
@group(0) @binding(5) var<uniform> frame_globals_raw: FrameGlobalsRaw;
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

// === Group 2: per-mesh geometry meta (vertex) — forked by instancing ===
{% if instancing_transforms %}
// Instanced shadow draws use uniform-with-dynamic-offset binding.
@group(2) @binding(0) var<uniform> geometry_mesh_meta: GeometryMeshMeta;
{% else %}
// Non-instanced shadow draws use the storage-array meta binding;
// `first_instance = mesh_meta_idx` is set per draw so
// `geometry_mesh_metas[instance_index]` resolves to this mesh's slot.
@group(2) @binding(0) var<storage, read> geometry_mesh_metas: array<GeometryMeshMeta>;
var<private> geometry_mesh_meta: GeometryMeshMeta;
{% endif %}

// === Group 3: morph + skin animation (vertex) — mirrors geometry bind_groups ===
@group(3) @binding(0) var<storage, read> geometry_morph_weights: array<f32>;
@group(3) @binding(1) var<storage, read> geometry_morph_values: array<f32>;
@group(3) @binding(2) var<storage, read> skin_joint_matrices: array<mat4x4<f32>>;
@group(3) @binding(3) var<storage, read> skin_joint_index_weights: array<f32>;
