fn vertex_color(attribute_data_offset: u32, triangle_indices: vec3<u32>, barycentric: vec3<f32>, color_info: VertexColorInfo, vertex_attribute_stride: u32, color_sets_index: u32) -> vec4<f32> {
{% if prep_read %}
    // Stage 2b: read the prep-materialized vertex-color array (parity-exact:
    // same barycentric + fp32 interp + visibility sample 0). Clamp the set
    // index to the cap.
    return textureLoad(prep_vcolor, g_prep_coords, i32(min(color_info.set_index, {{ max_prep_color_sets }}u - 1u)), 0);
{% else %}
    let color0 = _vertex_color_per_vertex(attribute_data_offset, color_info.set_index, triangle_indices.x, vertex_attribute_stride, color_sets_index);
    let color1 = _vertex_color_per_vertex(attribute_data_offset, color_info.set_index, triangle_indices.y, vertex_attribute_stride, color_sets_index);
    let color2 = _vertex_color_per_vertex(attribute_data_offset, color_info.set_index, triangle_indices.z, vertex_attribute_stride, color_sets_index);

    let interpolated_color = barycentric.x * color0 + barycentric.y * color1 + barycentric.z * color2;

    return interpolated_color;
{% endif %}
}

{# prep_read routes `vertex_color` to the prep array; the only other caller
   is the custom `material_vertex_color` accessor (emitted only for
   `base == Custom` + `inc.vertex_color`). Keep the recompute helper when
   either still needs it. #}
{% if !prep_read || (base == ShadingBase::Custom && inc.vertex_color) %}
fn _vertex_color_per_vertex(attribute_data_offset: u32, set_index: u32, vertex_index: u32, vertex_attribute_stride: u32, color_sets_index: u32) -> vec4<f32> {
    // First get to the right vertex, THEN to the right color set within that vertex.
    let vertex_start = attribute_data_offset + (vertex_index * vertex_attribute_stride);
    // `color_sets_index` is the float offset to COLOR_0 within the per-vertex
    // block (from `material_mesh_meta`) — colours pack *after* UVs, not at 0.
    // Each additional color set contributes 4 more floats per vertex.
    let color_offset = color_sets_index + (set_index * 4u);
    let index = vertex_start + color_offset;
    // attribute_data lives in the merged geometry pool aliased
    // here by `visibility_data` (binding 5).
    let color = vec4<f32>(visibility_data[index], visibility_data[index + 1], visibility_data[index + 2], visibility_data[index + 3]);

    return color;
}
{% endif %}{# not prep_read #}
