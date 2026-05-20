fn vertex_color(attribute_data_offset: u32, triangle_indices: vec3<u32>, barycentric: vec3<f32>, color_info: VertexColorInfo, vertex_attribute_stride: u32) -> vec4<f32> {
    let color0 = _vertex_color_per_vertex(attribute_data_offset, color_info.set_index, triangle_indices.x, vertex_attribute_stride);
    let color1 = _vertex_color_per_vertex(attribute_data_offset, color_info.set_index, triangle_indices.y, vertex_attribute_stride);
    let color2 = _vertex_color_per_vertex(attribute_data_offset, color_info.set_index, triangle_indices.z, vertex_attribute_stride);

    let interpolated_color = barycentric.x * color0 + barycentric.y * color1 + barycentric.z * color2;

    return interpolated_color;
}

fn _vertex_color_per_vertex(attribute_data_offset: u32, set_index: u32, vertex_index: u32, vertex_attribute_stride: u32) -> vec4<f32> {
    // First get to the right vertex, THEN to the right color set within that vertex
    let vertex_start = attribute_data_offset + (vertex_index * vertex_attribute_stride);
    // Color sets always start at offset 0 in the packed attribute stream.
    // Each additional color set contributes 4 more floats per vertex.
    let color_offset = set_index * 4u;
    let index = vertex_start + color_offset;
    // §16.E2: attribute_data lives in the merged geometry pool aliased
    // here by `visibility_data` (binding 5).
    let color = vec4<f32>(visibility_data[index], visibility_data[index + 1], visibility_data[index + 2], visibility_data[index + 3]);

    return color;
}
