/*************** START math.wgsl ******************/
{% include "shared_wgsl/math.wgsl" %}
/*************** END math.wgsl ******************/

/*************** START mesh_meta.wgsl ******************/
{% include "shared_wgsl/material_mesh_meta.wgsl" %}
/*************** END mesh_meta.wgsl ******************/

struct PickInput {
    mouse_x : i32,
    mouse_y : i32,
};
struct PickOutput {
    valid: u32,
    mesh_key_high: u32,
    mesh_key_low: u32,
};

{% if multisampled_geometry %}
    @group(0) @binding(0) var visibility_data_tex: texture_multisampled_2d<u32>;
{% else %}
    @group(0) @binding(0) var visibility_data_tex: texture_2d<u32>;
{% endif %}

@group(0) @binding(1) var<storage, read> material_mesh_metas: array<MaterialMeshMeta>;
@group(0) @binding(2) var<uniform> pick_input: PickInput;
@group(0) @binding(3) var<storage, read_write> pick_output: PickOutput;


@compute @workgroup_size(1, 1)
fn main() {
    let coords = vec2<i32>(pick_input.mouse_x, pick_input.mouse_y);
    let visibility_data_info = textureLoad(visibility_data_tex, coords, 0);
    let material_meta_offset = join32(visibility_data_info.z, visibility_data_info.w);
    let triangle_index = join32(visibility_data_info.x, visibility_data_info.y);

    if (triangle_index == U32_MAX) {
        pick_output.valid = 0u;
        pick_output.mesh_key_high = 0u;
        pick_output.mesh_key_low = 0;;
        return;
    }

    let material_mesh_meta = material_mesh_metas[material_meta_offset / META_SIZE_IN_BYTES];

    pick_output.valid = 1u;
    pick_output.mesh_key_high = material_mesh_meta.mesh_key_high;
    pick_output.mesh_key_low = material_mesh_meta.mesh_key_low;
}
