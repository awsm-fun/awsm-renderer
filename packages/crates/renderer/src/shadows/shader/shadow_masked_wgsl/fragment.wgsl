// Shared masking-alpha helpers (material loads, UV reconstruction, LOD-0 pool
// sample, custom alpha-only wrapper, and `mask_alpha_at`) — identical to the
// masked geometry fragment so the cutout test is byte-for-byte the same.
{% include "shared_wgsl/masked_alpha.wgsl" %}

// Depth-only cutout fragment: reconstruct the masking alpha at the pixel center
// and `discard` below the per-mesh cutoff so holes don't write shadow depth
// (later depth-tested receivers see through them). Binary discard — the shadow
// atlas is single-sampled (no MSAA / sample_mask) and PCF/PCSS softens the
// cutout edge at sample time. No color outputs: the rasterizer writes depth.
struct FragmentInput {
    @location(0) @interpolate(flat) triangle_index: u32,
    @location(1) barycentric: vec2<f32>,
    @location(2) @interpolate(flat) material_mesh_meta_offset: u32,
}

@fragment
fn fs_main(input: FragmentInput) {
    let mm = material_mesh_metas[input.material_mesh_meta_offset / META_SIZE_IN_BYTES];
    let material_offset = mm.material_offset;
    let vertex_attribute_stride = mm.vertex_attribute_stride / 4u;
    let attribute_indices_offset = mm.vertex_attribute_indices_offset / 4u;
    let attribute_data_offset = mm.vertex_attribute_data_offset / 4u;
    let uv_sets_index = mm.uv_sets_index;
    let color_sets_index = mm.color_sets_index;

    let base_triangle_index = attribute_indices_offset + (input.triangle_index * 3u);
    let triangle_indices = vec3<u32>(
        bitcast<u32>(visibility_data[base_triangle_index]),
        bitcast<u32>(visibility_data[base_triangle_index + 1u]),
        bitcast<u32>(visibility_data[base_triangle_index + 2u]),
    );

    let bary = vec3<f32>(input.barycentric.x, input.barycentric.y, 1.0 - input.barycentric.x - input.barycentric.y);
    let alpha = mask_alpha_at(bary, triangle_indices, attribute_data_offset, vertex_attribute_stride, uv_sets_index, color_sets_index, material_offset);
    if alpha < mm.alpha_cutoff {
        discard;
    }
}
