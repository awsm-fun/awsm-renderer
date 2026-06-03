struct ProjectedVertices {
    p0: VertexProjection,
    p1: VertexProjection,
    p2: VertexProjection
}

struct ObjectSpaceVertices {
    p0: vec3<f32>,
    p1: vec3<f32>,
    p2: vec3<f32>,
}

struct VertexProjection {
    screen: vec2<f32>,
    inv_w : f32,
    valid : bool,
}

fn project_vertices(camera: Camera, os: ObjectSpaceVertices, model_transform: mat4x4<f32>, screen_dims: vec2<f32>) -> ProjectedVertices {
    return ProjectedVertices(
        _project_vertex(camera, os.p0, model_transform, screen_dims),
        _project_vertex(camera, os.p1, model_transform, screen_dims),
        _project_vertex(camera, os.p2, model_transform, screen_dims),
    );
}

fn get_object_space_vertices(visibility_data_offset: u32, triangle_index: u32) -> ObjectSpaceVertices {
    return ObjectSpaceVertices(
        _get_vertex_position(visibility_data_offset, triangle_index, 0u),
        _get_vertex_position(visibility_data_offset, triangle_index, 1u),
        _get_vertex_position(visibility_data_offset, triangle_index, 2u),
    );
}

fn _get_vertex_position(visibility_data_offset: u32, triangle_index: u32, vertex_index: u32) -> vec3<f32> {
    // Visibility buffer layout per vertex (52 bytes = 13 floats):
    // - positions (vec3<f32>), 3 floats
    // - triangle_index (u32), 1 float
    // - barycentric (vec2<f32>), 2 floats
    // - normals (vec3<f32>), 3 floats
    // - tangents (vec4<f32>), 4 floats
    // Total: 3 + 1 + 2 + 3 + 4 = 13 floats per vertex
    const floats_per_vertex = 13u;
    const vertices_per_triangle = 3u;
    const floats_per_triangle = floats_per_vertex * vertices_per_triangle; // 39

    // first get to the right triangle
    let triangle_start = visibility_data_offset + (triangle_index * floats_per_triangle);
    // then each position for each vertex within that triangle
    let vertex_start = triangle_start + (vertex_index * floats_per_vertex);

    // Position is at offset 0 within each vertex
    return vec3<f32>(
        visibility_data[vertex_start],
        visibility_data[vertex_start + 1u],
        visibility_data[vertex_start + 2u]
    );
}

fn _project_vertex(
    camera: Camera,
    position_os: vec3<f32>,
    model_transform: mat4x4<f32>,
    screen_dims: vec2<f32>
) -> VertexProjection {
    let world = model_transform * vec4<f32>(position_os, 1.0);
    let clip  = camera.view_proj * world;

    if (abs(clip.w) < 1e-6) {
        return VertexProjection(vec2<f32>(0.0, 0.0), 0.0, false);
    }

    let inv_w = 1.0 / clip.w;
    let ndc   = clip.xy * inv_w;
    let uv    = vec2<f32>((ndc.x + 1.0) * 0.5, (1.0 - ndc.y) * 0.5);
    let pixel = uv * screen_dims;

    return VertexProjection(pixel, inv_w, true);
}
