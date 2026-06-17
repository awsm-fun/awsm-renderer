// Bind group declarations for the material prep compute pass (Plan B,
// docs/plans/deferred-shared-prep-pass.md). Layout must stay in lockstep with
// material_prep/bind_group.rs (added in the pipeline-wiring sub-stage).
//
// Inputs (read): visibility texture (triangle id + meta offset) + barycentric
// texture from the geometry pass, the merged geometry pool (`visibility_data`),
// and per-mesh metadata. Outputs (storage-write): interpolated UV0 + vertex
// color — the geometry-pool-fetch-heavy attributes the slim per-material shader
// would otherwise recompute. World position is NOT materialized (decision #2:
// the slim shader keeps the cheap depth-unprojection). Shadow visibility + edge
// outputs arrive in stages 3 / 5.

// Per-mesh metadata struct (defined here so the binding below can reference it;
// included once — the compute half references it after concatenation).
{% include "shared_wgsl/material_mesh_meta.wgsl" %}

// Visibility buffer (triangle id + meta offset), from the geometry pass.
@group(0) @binding(0) var visibility_data_tex: {% if multisampled_geometry %}texture_multisampled_2d<u32>{% else %}texture_2d<u32>{% endif %};
// Barycentric (RG = u16 fixed-point weights; BA = instance id).
@group(0) @binding(1) var barycentric_tex: {% if multisampled_geometry %}texture_multisampled_2d<u32>{% else %}texture_2d<u32>{% endif %};
// Merged geometry pool (positions / indices / vertex attributes), as f32 words.
@group(0) @binding(2) var<storage, read> visibility_data: array<f32>;
// Per-mesh metadata (offsets, strides, set indices).
@group(0) @binding(3) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;

// Materialized outputs (storage-write).
@group(0) @binding(4) var uv_out: texture_storage_2d<rg32float, write>;
@group(0) @binding(5) var vcolor_out: texture_storage_2d<rgba32float, write>;
