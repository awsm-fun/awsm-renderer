// Material decal pass bind groups.
//
// Group 0 — main inputs + output:
//   0  visibility_data_tex  (uint texture; per-pixel material_meta_offset)
//   1  depth_tex            (depth texture; per-pixel world-Z reconstruction)
//   2  opaque_tex_in        (float texture; already-shaded opaque output)
//   3  transparent_tex_out  (storage write; the blit just copied opaque → transparent;
//                            decals overwrite the destination pixel with the blended
//                            value. Non-decal-affected pixels are left untouched so the
//                            blit's content survives.)
//   4  material_mesh_metas  (storage RO)
//   5  decals_buffer        (storage RO; header + per-decal array)
//   6  camera_raw           (uniform; for `inv_view_proj` to unproject depth)
//
// Group 1 — texture pool (same layout as the opaque pass uses; decal
// textures sit alongside material textures in the same arrays).

/*************** START material_mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END material_mesh_meta.wgsl ******************/

/*************** START camera.wgsl ******************/
{% include "shared_wgsl/camera.wgsl" %}
/*************** END camera.wgsl ******************/

// `join32` — inlined helper to avoid double-including math.wgsl.
fn join32(hi: u32, lo: u32) -> u32 {
    return (hi << 16u) | lo;
}
const U32_MAX: u32 = 4294967295u;

{% if multisampled_geometry %}
@group(0) @binding(0) var visibility_data_tex: texture_multisampled_2d<u32>;
@group(0) @binding(1) var depth_tex: texture_depth_multisampled_2d;
{% else %}
@group(0) @binding(0) var visibility_data_tex: texture_2d<u32>;
@group(0) @binding(1) var depth_tex: texture_depth_2d;
{% endif %}

@group(0) @binding(2) var opaque_tex_in: texture_2d<f32>;
@group(0) @binding(3) var transparent_tex_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;

struct Decal {
    inverse_transform: mat4x4<f32>,
    texture_index: u32,
    alpha: f32,
    blend_mode: u32,
    _pad: u32,
};

// Packed decal storage. `count` lives in the header at offset 0; the
// per-decal array follows after a 16-byte vec4-alignment pad.
struct DecalsBuffer {
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    items: array<Decal>,
};
@group(0) @binding(5) var<storage, read> decals_buffer: DecalsBuffer;

@group(0) @binding(6) var<uniform> camera_raw: CameraRaw;

{% for i in 0..texture_pool_arrays_len %}
    @group(1) @binding({{ i }}u) var pool_tex_{{ i }}: texture_2d_array<f32>;
{% endfor %}
{% for i in 0..texture_pool_samplers_len %}
    @group(1) @binding({{ texture_pool_arrays_len + i }}u) var pool_sampler_{{ i }}: sampler;
{% endfor %}
