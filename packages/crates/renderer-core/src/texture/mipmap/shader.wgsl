@group(0) @binding(0) var input_texture: texture_2d_array<f32>;
@group(0) @binding(1) var input_sampler: sampler;
@group(0) @binding(2) var output_texture: texture_storage_2d_array<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let output_size = textureDimensions(output_texture);
    
    if (global_id.x >= output_size.x || global_id.y >= output_size.y) {
        return;
    }

    let depth = global_id.z;
    
    let input_size = textureDimensions(input_texture);
    
    // Calculate the pixel coordinates in the input texture that correspond to this output pixel
    // Each output pixel should sample from a 2x2 region in the input texture
    let input_coord = vec2<f32>(global_id.xy) * 2.0 + 0.5;
    
    // Convert to UV coordinates for sampling
    let base_uv = input_coord / vec2<f32>(input_size);
    
    // Calculate texel size for offsetting
    let texel_size = 1.0 / vec2<f32>(input_size);
    
    // Sample a 2x2 box filter region
    // Sample at the corners of a 2x2 pixel region
    let sample_00 = textureSampleLevel(input_texture, input_sampler, base_uv + vec2<f32>(-0.5, -0.5) * texel_size, depth, 0.0);
    let sample_01 = textureSampleLevel(input_texture, input_sampler, base_uv + vec2<f32>(0.5, -0.5) * texel_size, depth, 0.0);
    let sample_10 = textureSampleLevel(input_texture, input_sampler, base_uv + vec2<f32>(-0.5, 0.5) * texel_size, depth, 0.0);
    let sample_11 = textureSampleLevel(input_texture, input_sampler, base_uv + vec2<f32>(0.5, 0.5) * texel_size, depth, 0.0);
    
    // Average the samples (simple box filter)
    let result = (sample_00 + sample_01 + sample_10 + sample_11) * 0.25;
    
    textureStore(output_texture, vec2<i32>(global_id.xy), depth, result);
}