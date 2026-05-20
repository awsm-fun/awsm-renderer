// Material classify pass bind groups.
//
// One bind group:
//   0: visibility_data_tex — sampled to recover per-pixel material id.
//   1: material_mesh_metas — storage[RO] mesh-meta table for the
//      `material_meta_offset → material_offset` step.
//   2: materials_data     — storage[RO] material payload; the first
//      `u32` of each entry is the `shader_id`.
//   3: classify_output    — storage[RW] (atomic) buckets + indirect
//      args. Layout matches the
//      `ClassifyBuffers` Rust-side header verbatim.

/*************** START material_mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END material_mesh_meta.wgsl ******************/

// `join32` is the same helper math.wgsl exposes; inlined here to
// avoid pulling math.wgsl into both `bind_groups` and `compute`,
// which double-declares its `const U32_MAX`. Keep the body in
// lockstep with `shared_wgsl/math.wgsl::join32` if that ever changes.
fn join32(hi: u32, lo: u32) -> u32 {
    return (hi << 16u) | lo;
}
const U32_MAX: u32 = 4294967295u;

{% if multisampled_geometry %}
@group(0) @binding(0) var visibility_data_tex: texture_multisampled_2d<u32>;
{% else %}
@group(0) @binding(0) var visibility_data_tex: texture_2d<u32>;
{% endif %}

@group(0) @binding(1) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;

@group(0) @binding(2) var<storage, read> materials_data: array<u32>;

struct ClassifyIndirectArgs {
    workgroup_count_x: atomic<u32>,
    workgroup_count_y: u32,
    workgroup_count_z: u32,
    _pad: u32,
};

// Storage-buffer layout — must stay in lockstep with the byte writer
// in `material_classify::buffers::write_header`. The three indirect
// args slots are at offsets 0/16/32; `dispatchWorkgroupsIndirect`
// reads each as `(x, y, z)` from the bound buffer.
struct ClassifyOutput {
    args_pbr: ClassifyIndirectArgs,
    args_unlit: ClassifyIndirectArgs,
    args_toon: ClassifyIndirectArgs,
    pbr_offset: u32,
    unlit_offset: u32,
    toon_offset: u32,
    bucket_capacity: u32,
    tiles: array<vec2<u32>>,
};

@group(0) @binding(3) var<storage, read_write> classify_output: ClassifyOutput;
