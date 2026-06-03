// 20-byte packed texture descriptor suitable for uniform/storage buffers.
//
// A bit in `extra` indicates whether the texture is actually used at actually
// (instead of sentinal values or flags on the material)
//
// Layout:
// - size:                width/height in texels (16 bits each)
// - array_and_layer:     array texture index (12 bits), layer index (20 bits)
// - uv_and_sampler:      uv set index (8 bits), sampler index (24 bits)
// - extra:               flags (8 bits), address_mode_u (8 bits),
//                        address_mode_v (8 bits), padding (8 bits)
//                        (address modes used for mipmap gradient calculation only)
// - transform_offset:    byte offset into texture transforms buffer (32 bits)
//
// Notes:
// - 16 bits for width/height covers all practical WebGPU limits.
// - 12 bits for array_index => up to 4096 texture arrays.
// - 20 bits for layer_index => up to 1,048,576 layers (way above spec limits).
// - 8 bits for uv_set_index => up to 256 UV sets (you'll use < 8).
// - 24 bits for sampler_index => up to ~16M samplers (effectively unlimited).
// - flags byte: bit 0 = has mipmaps; rest reserved.
// - 32 bits for transform_offset => supports millions of transforms
struct TextureInfoRaw {
    // packed: width (low 16 bits), height (high 16 bits)
    size: u32,

    // packed:
    //   bits  0..11 : array_index
    //   bits 12..31 : layer_index
    array_and_layer: u32,

    // packed:
    //   bits  0..7  : uv_set_index
    //   bits  8..31 : sampler_index
    uv_and_sampler: u32,

    // packed:
    //   bits  0..7  : flags
    //                  bit 0 -> has texture
    //                  bit 1 -> has mipmaps
    //                  bits 2..7 reserved
    //   bits  8..15 : address_mode_u (for mipmap gradient calculation)
    //   bits 16..23 : address_mode_v (for mipmap gradient calculation)
    //   bits 24..31 : padding / reserved
    extra: u32,

    // byte offset into texture transforms buffer (full 32 bits)
    transform_offset: u32,
};

struct TextureInfo {
    exists: bool,
    size: vec2<u32>,   // (width, height)
    array_index: u32,
    layer_index: u32,
    uv_set_index: u32,
    sampler_index: u32,
    mipmapped: bool,
    address_mode_u: u32,
    address_mode_v: u32,
    uv_transform_index: u32,
};

struct TextureTransform {
    // M = [ m00  m01 ]
    //     [ m10  m11 ]
    // stored as vec4: (m00, m01, m10, m11)
    m: vec4<f32>,

    // B = offset + origin - M * origin
    b: vec2<f32>,
    _pad: vec2<f32>, // keep 32 bytes total
};

fn convert_texture_info(raw: TextureInfoRaw) -> TextureInfo {
    const BITMASK_EXISTS: u32 = 1u;
    const BITMASK_MIPMAPPED: u32 = 1u << 1u;

    // size
    let width:  u32 = raw.size & 0xFFFFu;
    let height: u32 = raw.size >> 16u;

    // array index (12 bits) and layer index (20 bits)
    let array_index: u32 =  raw.array_and_layer & 0xFFFu;      // bits 0..11
    let layer_index: u32 =  raw.array_and_layer >> 12u;        // bits 12..31

    // uv set (8 bits) and sampler index (24 bits)
    let uv_set_index:  u32 =  raw.uv_and_sampler & 0xFFu;      // bits 0..7
    let sampler_index: u32 =  raw.uv_and_sampler >> 8u;        // bits 8..31

    // flags + address modes
    let flags: u32          = raw.extra & 0xFFu;                // bits 0..7
    let exists: bool     = (flags & BITMASK_EXISTS) != 0u;
    let mipmapped: bool     = (flags & BITMASK_MIPMAPPED) != 0u;

    let address_mode_u: u32 = (raw.extra >> 8u)  & 0xFFu;       // bits 8..15
    let address_mode_v: u32 = (raw.extra >> 16u) & 0xFFu;       // bits 16..23

    // Convert byte offset to array index (each transform is 32 bytes)
    let uv_transform_index: u32 = raw.transform_offset / 32u;

    return TextureInfo(
        exists,
        vec2<u32>(width, height),
        array_index,
        layer_index,
        uv_set_index,
        sampler_index,
        mipmapped,
        address_mode_u,
        address_mode_v,
        uv_transform_index
    );
}

fn texture_info_none() -> TextureInfo {
    return TextureInfo(
        false,
        vec2<u32>(0u, 0u),
        0u,
        0u,
        0u,
        0u,
        false,
        0u,
        0u,
        0u
    );
}

fn texture_transform_uvs(
    uv: vec2<f32>,
    tex_info: TextureInfo
) -> vec2<f32> {
    // CPU assigns index to identity if needed, no special branch required.
    let t = texture_transforms[tex_info.uv_transform_index];

    let m00 = t.m.x;
    let m01 = t.m.y;
    let m10 = t.m.z;
    let m11 = t.m.w;
    let B   = t.b;

    let uv_transformed = vec2<f32>(
        m00 * uv.x + m01 * uv.y,
        m10 * uv.x + m11 * uv.y
    ) + B;

    return uv_transformed;
}
