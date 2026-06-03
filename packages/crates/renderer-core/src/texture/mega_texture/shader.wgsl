// Write the image atlas entry to the atlas texture, with bleeding for the padding
// dispatch with (src_width + 2*padding, src_height + 2*padding, 1)
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var dst_atlas_tex: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(2) var<uniform> entry: ImageAtlasEntry;

struct ImageAtlasEntry {
    pixel_offset: vec2<u32>, // This is the offset where the image will be placed in the atlas, past padding
    padding: u32,
    layer_index: u32,
    is_srgb_encoded: u32, // 1 for baseColor/emissive PNG/JPG, 0 for ORM/normal
};

@compute @workgroup_size(8,8)
fn main(@builtin(global_invocation_id) gid : vec3<u32>) {

    let src_size = textureDimensions(src_tex);
    let padded_size = src_size + vec2<u32>(entry.padding * 2u);

    // Bounds check for the padded area we're writing
    if (gid.x >= padded_size.x || gid.y >= padded_size.y) {
        return;
    }

    // Calculate source coordinate with edge clamping for padding
    let src_coord = clamp(
        // Value: Transform padded coord to source coord (may go outside bounds)
        // so, for example if our x,y cooredinate is at 0,0, it becomes -padding, -padding
        // and if our x,y coordinate is at (src_width + 2*padding, src_height + 2*padding)
        // it becomes (src_width + padding, src_height +padding)
        // in other words, it shifts our coordintaes to be exactly the original plus padding *on either side*
        // and then we can calmp that so for the amount of padding on either side, we stay on the edge
        vec2<i32>(gid.xy) - vec2<i32>(i32(entry.padding)),
        // Min: Clamp negative coords to 0,0 (so for the full -padding area, we stay on the edge)
        vec2<i32>(0),
        // Max: Clamp beyond-bounds coords to (width,height) (minus 1 for zero-based indexing)
        // This ensures that if we go beyond the source texture bounds, we stay on the edge
        vec2<i32>(src_size - vec2<u32>(1u))
    );

    // Load the source color (with edge clamping for padding)
    var src_color = textureLoad(src_tex, src_coord, 0);

    if (entry.is_srgb_encoded != 0u) {
        src_color = vec4<f32>(srgb_to_linear3(src_color.rgb), src_color.a);
    }

    // Calculate destination position
    let dst_xy = entry.pixel_offset - vec2<u32>(entry.padding) + gid.xy;

    let atlas_size = textureDimensions(dst_atlas_tex);

    // Store to atlas texture
    textureStore(dst_atlas_tex, dst_xy, entry.layer_index, src_color);
}

fn srgb_to_linear3(c: vec3<f32>) -> vec3<f32> {
    let cutoff = step(vec3<f32>(0.04045), c);
    let low  = c / 12.92;
    let high = pow(max((c + 0.055) / 1.055, vec3<f32>(0.0)), vec3<f32>(2.4));
    return mix(low, high, cutoff);
}
