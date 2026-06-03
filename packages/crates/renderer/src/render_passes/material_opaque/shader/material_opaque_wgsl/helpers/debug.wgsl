// Debug visualization helpers
// These can be included conditionally and inserted into compute.wgsl where needed

// Debug normal visualization
fn debug_normals(material_color_normal: vec3<f32>) -> vec3<f32> {
    // Visualize normals as RGB (map from [-1,1] to [0,1])
    let n = safe_normalize(material_color_normal);
    return n * 0.5 + 0.5;
}
