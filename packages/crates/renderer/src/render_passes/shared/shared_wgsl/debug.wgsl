fn debug_test(tex: texture_2d_array<f32>, layer: u32, display_coords: vec2<i32>, display_dimensions: vec2<u32>) -> vec4<f32> {
    let texture_dimensions = textureDimensions(tex);
    
    // Scale display coordinates to texture space
    let texture_uv = vec2<f32>(display_coords) / vec2<f32>(display_dimensions);
    let texture_coords = vec2<i32>(texture_uv * vec2<f32>(texture_dimensions));
    
    // Sample the texture at the scaled coordinates
    let sample = textureLoad(tex, texture_coords, layer, 0);
    
    // If the sample is black, show a debug pattern so we know the scaling is working
    if (sample.r == 0.0 && sample.g == 0.0 && sample.b == 0.0 && sample.a == 0.0) {
        // Show a simple grid pattern so you know the coordinate mapping works
        let grid_size = 64;
        let grid = ((texture_coords.x / grid_size) + (texture_coords.y / grid_size)) % 2;
        return select(
            vec4<f32>(0.1, 0.1, 0.1, 1.0),  // Dark gray
            vec4<f32>(0.3, 0.3, 0.3, 1.0),  // Light gray
            grid == 0
        );
    }
    
    return sample;
}