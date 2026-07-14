//! Block-compressed (and uncompressed) texture format layout helpers +
//! KTX2→WebGPU format mapping — shared by the cubemap KTX2 loader and the
//! material texture upload path (KTX2/Basis transcode targets).
//!
//! Lifted from `cubemap/ktx.rs`, which now delegates here.

use crate::texture::TextureFormat;

#[inline]
fn align_up(x: u32, align: u32) -> u32 {
    debug_assert!(align.is_power_of_two());
    (x + (align - 1)) & !(align - 1)
}

/// Whether `format` is a block-compressed (BC / ETC2 / EAC / ASTC) format.
/// Block-compressed textures must be uploaded with pre-supplied mips and can
/// never go through the compute mip-gen or `srgb_to_linear` passes.
pub fn is_block_compressed(format: TextureFormat) -> bool {
    block_dims(format).is_some()
}

/// `(block_width, block_height, bytes_per_block)` for block-compressed
/// formats, `None` for uncompressed ones.
pub fn block_dims(format: TextureFormat) -> Option<(u32, u32, u32)> {
    Some(match format {
        TextureFormat::Bc1RgbaUnorm
        | TextureFormat::Bc1RgbaUnormSrgb
        | TextureFormat::Bc4RUnorm
        | TextureFormat::Bc4RSnorm
        | TextureFormat::Etc2Rgb8unorm
        | TextureFormat::Etc2Rgb8unormSrgb
        | TextureFormat::Etc2Rgb8a1unorm
        | TextureFormat::Etc2Rgb8a1unormSrgb
        | TextureFormat::EacR11unorm
        | TextureFormat::EacR11snorm => (4, 4, 8),

        TextureFormat::Bc2RgbaUnorm
        | TextureFormat::Bc2RgbaUnormSrgb
        | TextureFormat::Bc3RgbaUnorm
        | TextureFormat::Bc3RgbaUnormSrgb
        | TextureFormat::Bc5RgUnorm
        | TextureFormat::Bc5RgSnorm
        | TextureFormat::Bc6hRgbUfloat
        | TextureFormat::Bc6hRgbFloat
        | TextureFormat::Bc7RgbaUnorm
        | TextureFormat::Bc7RgbaUnormSrgb
        | TextureFormat::Etc2Rgba8unorm
        | TextureFormat::Etc2Rgba8unormSrgb
        | TextureFormat::EacRg11unorm
        | TextureFormat::EacRg11snorm
        | TextureFormat::Astc4x4Unorm
        | TextureFormat::Astc4x4UnormSrgb => (4, 4, 16),

        TextureFormat::Astc5x4Unorm | TextureFormat::Astc5x4UnormSrgb => (5, 4, 16),
        TextureFormat::Astc5x5Unorm | TextureFormat::Astc5x5UnormSrgb => (5, 5, 16),
        TextureFormat::Astc6x5Unorm | TextureFormat::Astc6x5UnormSrgb => (6, 5, 16),
        TextureFormat::Astc6x6Unorm | TextureFormat::Astc6x6UnormSrgb => (6, 6, 16),
        TextureFormat::Astc8x5Unorm | TextureFormat::Astc8x5UnormSrgb => (8, 5, 16),
        TextureFormat::Astc8x6Unorm | TextureFormat::Astc8x6UnormSrgb => (8, 6, 16),
        TextureFormat::Astc8x8Unorm | TextureFormat::Astc8x8UnormSrgb => (8, 8, 16),
        TextureFormat::Astc10x5Unorm | TextureFormat::Astc10x5UnormSrgb => (10, 5, 16),
        TextureFormat::Astc10x6Unorm | TextureFormat::Astc10x6UnormSrgb => (10, 6, 16),
        TextureFormat::Astc10x8Unorm | TextureFormat::Astc10x8UnormSrgb => (10, 8, 16),
        TextureFormat::Astc10x10Unorm | TextureFormat::Astc10x10UnormSrgb => (10, 10, 16),
        TextureFormat::Astc12x10Unorm | TextureFormat::Astc12x10UnormSrgb => (12, 10, 16),
        TextureFormat::Astc12x12Unorm | TextureFormat::Astc12x12UnormSrgb => (12, 12, 16),

        _ => return None,
    })
}

/// Bytes per pixel for uncompressed formats (block-compressed formats are
/// handled via [`block_dims`]; callers should branch on that first).
pub fn bytes_per_pixel(format: TextureFormat) -> u32 {
    match format {
        // 8-bit formats (1 byte per channel)
        TextureFormat::R8unorm
        | TextureFormat::R8snorm
        | TextureFormat::R8uint
        | TextureFormat::R8sint => 1,

        // 16-bit formats (2 bytes per channel)
        TextureFormat::R16uint | TextureFormat::R16sint | TextureFormat::R16float => 2,
        TextureFormat::Rg8unorm
        | TextureFormat::Rg8snorm
        | TextureFormat::Rg8uint
        | TextureFormat::Rg8sint => 2,

        // 32-bit formats (4 bytes per channel)
        TextureFormat::R32uint | TextureFormat::R32sint | TextureFormat::R32float => 4,
        TextureFormat::Rg16uint | TextureFormat::Rg16sint | TextureFormat::Rg16float => 4,
        TextureFormat::Rgba8unorm
        | TextureFormat::Rgba8unormSrgb
        | TextureFormat::Rgba8snorm
        | TextureFormat::Rgba8uint
        | TextureFormat::Rgba8sint => 4,
        TextureFormat::Bgra8unorm | TextureFormat::Bgra8unormSrgb => 4,
        TextureFormat::Rgb10a2unorm | TextureFormat::Rgb10a2uint => 4,
        TextureFormat::Rg11b10ufloat => 4,
        TextureFormat::Rgb9e5ufloat => 4,

        // 64-bit formats (8 bytes per channel)
        TextureFormat::Rg32uint | TextureFormat::Rg32sint | TextureFormat::Rg32float => 8,
        TextureFormat::Rgba16uint | TextureFormat::Rgba16sint | TextureFormat::Rgba16float => 8,

        // 128-bit formats (16 bytes per channel)
        TextureFormat::Rgba32uint | TextureFormat::Rgba32sint | TextureFormat::Rgba32float => 16,

        // Depth/stencil formats
        TextureFormat::Stencil8 => 1,
        TextureFormat::Depth16unorm => 2,
        TextureFormat::Depth24plus => 4,
        TextureFormat::Depth24plusStencil8 => 4,
        TextureFormat::Depth32float => 4,
        TextureFormat::Depth32floatStencil8 => 8,

        // Default fallback for any unhandled formats
        _ => 4,
    }
}

/// The tight (unpadded) bytes-per-row for one mip row of `width` texels:
/// block columns × bytes-per-block for compressed formats, texels ×
/// bytes-per-pixel otherwise. This is what `queue.writeTexture` accepts —
/// only *buffer*-to-texture copies need the 256 alignment of
/// [`aligned_bytes_per_row`].
pub fn tight_bytes_per_row(format: TextureFormat, width: u32) -> u32 {
    if let Some((bw, _bh, bpb)) = block_dims(format) {
        width.div_ceil(bw) * bpb
    } else {
        width * bytes_per_pixel(format)
    }
}

/// [`tight_bytes_per_row`] rounded up to WebGPU's 256-byte row alignment —
/// required for `copyBufferToTexture` staging paths (and used by the cubemap
/// loader's `writeTexture` calls, where the extra padding is merely wasteful,
/// not wrong).
pub fn aligned_bytes_per_row(format: TextureFormat, width: u32) -> u32 {
    align_up(tight_bytes_per_row(format, width), 256)
}

/// Rows-per-image in layout units: block rows for compressed formats, texel
/// rows otherwise.
pub fn rows_per_image(format: TextureFormat, height: u32) -> u32 {
    if let Some((_bw, bh, _bpb)) = block_dims(format) {
        height.div_ceil(bh)
    } else {
        height
    }
}

/// Tight byte size of one full mip level (`width`×`height` texels, one array
/// layer) — for validating transcoder/container output before upload.
pub fn mip_level_byte_size(format: TextureFormat, width: u32, height: u32) -> usize {
    tight_bytes_per_row(format, width) as usize * rows_per_image(format, height) as usize
}

/// The sRGB-decoding sibling of a linear block/color format, or `None` when
/// no sibling exists (two-channel data formats, formats that are already
/// sRGB). Compressed bytes are identical between the pair — only the
/// sampler-decode semantics differ — so a transcode result can be stored
/// sRGB-agnostic under the linear format and swapped to the sRGB variant at
/// bind time when the material slot carries color data.
pub fn srgb_variant(format: TextureFormat) -> Option<TextureFormat> {
    Some(match format {
        TextureFormat::Rgba8unorm => TextureFormat::Rgba8unormSrgb,
        TextureFormat::Bgra8unorm => TextureFormat::Bgra8unormSrgb,
        TextureFormat::Bc1RgbaUnorm => TextureFormat::Bc1RgbaUnormSrgb,
        TextureFormat::Bc2RgbaUnorm => TextureFormat::Bc2RgbaUnormSrgb,
        TextureFormat::Bc3RgbaUnorm => TextureFormat::Bc3RgbaUnormSrgb,
        TextureFormat::Bc7RgbaUnorm => TextureFormat::Bc7RgbaUnormSrgb,
        TextureFormat::Etc2Rgb8unorm => TextureFormat::Etc2Rgb8unormSrgb,
        TextureFormat::Etc2Rgb8a1unorm => TextureFormat::Etc2Rgb8a1unormSrgb,
        TextureFormat::Etc2Rgba8unorm => TextureFormat::Etc2Rgba8unormSrgb,
        TextureFormat::Astc4x4Unorm => TextureFormat::Astc4x4UnormSrgb,
        _ => return None,
    })
}

/// Maps a KTX2 (Vulkan) format to the WebGPU texture format, or `None` when
/// WebGPU has no equivalent.
#[cfg(feature = "ktx")]
pub fn map_ktx_format(format: ktx2::Format) -> Option<TextureFormat> {
    Some(match format {
        // ------------------------
        // 8-bit uncompressed
        // ------------------------
        ktx2::Format::R8_UNORM => TextureFormat::R8unorm,
        ktx2::Format::R8_SNORM => TextureFormat::R8snorm,
        ktx2::Format::R8_UINT => TextureFormat::R8uint,
        ktx2::Format::R8_SINT => TextureFormat::R8sint,
        // No R8 SRGB in WebGPU
        ktx2::Format::R8G8_UNORM => TextureFormat::Rg8unorm,
        ktx2::Format::R8G8_SNORM => TextureFormat::Rg8snorm,
        ktx2::Format::R8G8_UINT => TextureFormat::Rg8uint,
        ktx2::Format::R8G8_SINT => TextureFormat::Rg8sint,
        // No RG8 SRGB in WebGPU

        // 24-bit RGB (unsupported in WebGPU)
        ktx2::Format::R8G8B8_UNORM
        | ktx2::Format::R8G8B8_SNORM
        | ktx2::Format::R8G8B8_UINT
        | ktx2::Format::R8G8B8_SINT
        | ktx2::Format::R8G8B8_SRGB
        | ktx2::Format::B8G8R8_UNORM
        | ktx2::Format::B8G8R8_SNORM
        | ktx2::Format::B8G8R8_UINT
        | ktx2::Format::B8G8R8_SINT
        | ktx2::Format::B8G8R8_SRGB => return None,

        // 32-bit RGBA
        ktx2::Format::R8G8B8A8_UNORM => TextureFormat::Rgba8unorm,
        ktx2::Format::R8G8B8A8_SNORM => TextureFormat::Rgba8snorm,
        ktx2::Format::R8G8B8A8_UINT => TextureFormat::Rgba8uint,
        ktx2::Format::R8G8B8A8_SINT => TextureFormat::Rgba8sint,
        ktx2::Format::R8G8B8A8_SRGB => TextureFormat::Rgba8unormSrgb,

        // 32-bit BGRA (only UNORM + SRGB supported)
        ktx2::Format::B8G8R8A8_UNORM => TextureFormat::Bgra8unorm,
        ktx2::Format::B8G8R8A8_SRGB => TextureFormat::Bgra8unormSrgb,
        ktx2::Format::B8G8R8A8_SNORM
        | ktx2::Format::B8G8R8A8_UINT
        | ktx2::Format::B8G8R8A8_SINT => return None,

        // 10:10:10:2
        // WebGPU supports "Rgb10a2unorm" and "Rgb10a2uint" in RGBA order.
        // Only map the KTX variant whose channel order matches RGBA.
        ktx2::Format::A2R10G10B10_UNORM_PACK32 => TextureFormat::Rgb10a2unorm,
        ktx2::Format::A2R10G10B10_UINT_PACK32 => TextureFormat::Rgb10a2uint,
        // The ABGR-ordered variants don't match WebGPU's channel order.
        ktx2::Format::A2R10G10B10_SNORM_PACK32
        | ktx2::Format::A2R10G10B10_SINT_PACK32
        | ktx2::Format::A2B10G10R10_UNORM_PACK32
        | ktx2::Format::A2B10G10R10_SNORM_PACK32
        | ktx2::Format::A2B10G10R10_UINT_PACK32
        | ktx2::Format::A2B10G10R10_SINT_PACK32 => return None,

        // 16-bit scalar/vector (only uint/sint/float are in WebGPU)
        ktx2::Format::R16_UINT => TextureFormat::R16uint,
        ktx2::Format::R16_SINT => TextureFormat::R16sint,
        ktx2::Format::R16_SFLOAT => TextureFormat::R16float,
        // No R16_UNORM/SNORM in WebGPU
        ktx2::Format::R16_UNORM | ktx2::Format::R16_SNORM => return None,

        ktx2::Format::R16G16_UINT => TextureFormat::Rg16uint,
        ktx2::Format::R16G16_SINT => TextureFormat::Rg16sint,
        ktx2::Format::R16G16_SFLOAT => TextureFormat::Rg16float,
        // No RG16 UNORM/SNORM
        ktx2::Format::R16G16_UNORM | ktx2::Format::R16G16_SNORM => return None,

        // 16-bit RGB (not supported as plain RGB in WebGPU)
        ktx2::Format::R16G16B16_UNORM
        | ktx2::Format::R16G16B16_SNORM
        | ktx2::Format::R16G16B16_UINT
        | ktx2::Format::R16G16B16_SINT
        | ktx2::Format::R16G16B16_SFLOAT => return None,

        // 16-bit RGBA
        ktx2::Format::R16G16B16A16_UINT => TextureFormat::Rgba16uint,
        ktx2::Format::R16G16B16A16_SINT => TextureFormat::Rgba16sint,
        ktx2::Format::R16G16B16A16_SFLOAT => TextureFormat::Rgba16float,
        // No UNORM/SNORM variants
        ktx2::Format::R16G16B16A16_UNORM | ktx2::Format::R16G16B16A16_SNORM => return None,

        // 32-bit scalar/vector
        ktx2::Format::R32_UINT => TextureFormat::R32uint,
        ktx2::Format::R32_SINT => TextureFormat::R32sint,
        ktx2::Format::R32_SFLOAT => TextureFormat::R32float,

        ktx2::Format::R32G32_UINT => TextureFormat::Rg32uint,
        ktx2::Format::R32G32_SINT => TextureFormat::Rg32sint,
        ktx2::Format::R32G32_SFLOAT => TextureFormat::Rg32float,

        // 32-bit RGB (not supported as plain RGB in WebGPU)
        ktx2::Format::R32G32B32_UINT
        | ktx2::Format::R32G32B32_SINT
        | ktx2::Format::R32G32B32_SFLOAT => return None,

        ktx2::Format::R32G32B32A32_UINT => TextureFormat::Rgba32uint,
        ktx2::Format::R32G32B32A32_SINT => TextureFormat::Rgba32sint,
        ktx2::Format::R32G32B32A32_SFLOAT => TextureFormat::Rgba32float,

        // 64-bit formats are not supported in WebGPU
        ktx2::Format::R64_UINT
        | ktx2::Format::R64_SINT
        | ktx2::Format::R64_SFLOAT
        | ktx2::Format::R64G64_UINT
        | ktx2::Format::R64G64_SINT
        | ktx2::Format::R64G64_SFLOAT
        | ktx2::Format::R64G64B64_UINT
        | ktx2::Format::R64G64B64_SINT
        | ktx2::Format::R64G64B64_SFLOAT
        | ktx2::Format::R64G64B64A64_UINT
        | ktx2::Format::R64G64B64A64_SINT
        | ktx2::Format::R64G64B64A64_SFLOAT => return None,

        // Special packed floats
        ktx2::Format::B10G11R11_UFLOAT_PACK32 => TextureFormat::Rg11b10ufloat,
        ktx2::Format::E5B9G9R9_UFLOAT_PACK32 => TextureFormat::Rgb9e5ufloat,

        // Depth / Stencil
        ktx2::Format::D16_UNORM => TextureFormat::Depth16unorm,
        // KTX "X8_D24_UNORM_PACK32" is a 24-bit depth; WebGPU exposes "Depth24plus" (implementation-chosen 24-bit-ish).
        ktx2::Format::X8_D24_UNORM_PACK32 => TextureFormat::Depth24plus,
        ktx2::Format::D32_SFLOAT => TextureFormat::Depth32float,
        ktx2::Format::S8_UINT => TextureFormat::Stencil8,

        // Combined DS: map only to ones WebGPU actually has.
        // D16S8 is not available; D24S8 becomes Depth24plusStencil8; D32FS8 has a direct match.
        ktx2::Format::D16_UNORM_S8_UINT => return None,
        ktx2::Format::D24_UNORM_S8_UINT => TextureFormat::Depth24plusStencil8,
        ktx2::Format::D32_SFLOAT_S8_UINT => TextureFormat::Depth32floatStencil8,

        // Block compression: BC / ETC2 / EAC
        // Note: BC1 "RGB" and "RGBA" are the same container; WebGPU exposes the RGBA spelling.
        ktx2::Format::BC1_RGB_UNORM_BLOCK | ktx2::Format::BC1_RGBA_UNORM_BLOCK => {
            TextureFormat::Bc1RgbaUnorm
        }
        ktx2::Format::BC1_RGB_SRGB_BLOCK | ktx2::Format::BC1_RGBA_SRGB_BLOCK => {
            TextureFormat::Bc1RgbaUnormSrgb
        }

        ktx2::Format::BC2_UNORM_BLOCK => TextureFormat::Bc2RgbaUnorm,
        ktx2::Format::BC2_SRGB_BLOCK => TextureFormat::Bc2RgbaUnormSrgb,
        ktx2::Format::BC3_UNORM_BLOCK => TextureFormat::Bc3RgbaUnorm,
        ktx2::Format::BC3_SRGB_BLOCK => TextureFormat::Bc3RgbaUnormSrgb,
        ktx2::Format::BC4_UNORM_BLOCK => TextureFormat::Bc4RUnorm,
        ktx2::Format::BC4_SNORM_BLOCK => TextureFormat::Bc4RSnorm,
        ktx2::Format::BC5_UNORM_BLOCK => TextureFormat::Bc5RgUnorm,
        ktx2::Format::BC5_SNORM_BLOCK => TextureFormat::Bc5RgSnorm,
        ktx2::Format::BC6H_UFLOAT_BLOCK => TextureFormat::Bc6hRgbUfloat,
        ktx2::Format::BC6H_SFLOAT_BLOCK => TextureFormat::Bc6hRgbFloat,
        ktx2::Format::BC7_UNORM_BLOCK => TextureFormat::Bc7RgbaUnorm,
        ktx2::Format::BC7_SRGB_BLOCK => TextureFormat::Bc7RgbaUnormSrgb,

        ktx2::Format::ETC2_R8G8B8_UNORM_BLOCK => TextureFormat::Etc2Rgb8unorm,
        ktx2::Format::ETC2_R8G8B8_SRGB_BLOCK => TextureFormat::Etc2Rgb8unormSrgb,
        ktx2::Format::ETC2_R8G8B8A1_UNORM_BLOCK => TextureFormat::Etc2Rgb8a1unorm,
        ktx2::Format::ETC2_R8G8B8A1_SRGB_BLOCK => TextureFormat::Etc2Rgb8a1unormSrgb,
        ktx2::Format::ETC2_R8G8B8A8_UNORM_BLOCK => TextureFormat::Etc2Rgba8unorm,
        ktx2::Format::ETC2_R8G8B8A8_SRGB_BLOCK => TextureFormat::Etc2Rgba8unormSrgb,
        ktx2::Format::EAC_R11_UNORM_BLOCK => TextureFormat::EacR11unorm,
        ktx2::Format::EAC_R11_SNORM_BLOCK => TextureFormat::EacR11snorm,
        ktx2::Format::EAC_R11G11_UNORM_BLOCK => TextureFormat::EacRg11unorm,
        ktx2::Format::EAC_R11G11_SNORM_BLOCK => TextureFormat::EacRg11snorm,

        // ASTC LDR (UNORM / SRGB)
        ktx2::Format::ASTC_4x4_UNORM_BLOCK => TextureFormat::Astc4x4Unorm,
        ktx2::Format::ASTC_4x4_SRGB_BLOCK => TextureFormat::Astc4x4UnormSrgb,
        ktx2::Format::ASTC_5x4_UNORM_BLOCK => TextureFormat::Astc5x4Unorm,
        ktx2::Format::ASTC_5x4_SRGB_BLOCK => TextureFormat::Astc5x4UnormSrgb,
        ktx2::Format::ASTC_5x5_UNORM_BLOCK => TextureFormat::Astc5x5Unorm,
        ktx2::Format::ASTC_5x5_SRGB_BLOCK => TextureFormat::Astc5x5UnormSrgb,
        ktx2::Format::ASTC_6x5_UNORM_BLOCK => TextureFormat::Astc6x5Unorm,
        ktx2::Format::ASTC_6x5_SRGB_BLOCK => TextureFormat::Astc6x5UnormSrgb,
        ktx2::Format::ASTC_6x6_UNORM_BLOCK => TextureFormat::Astc6x6Unorm,
        ktx2::Format::ASTC_6x6_SRGB_BLOCK => TextureFormat::Astc6x6UnormSrgb,
        ktx2::Format::ASTC_8x5_UNORM_BLOCK => TextureFormat::Astc8x5Unorm,
        ktx2::Format::ASTC_8x5_SRGB_BLOCK => TextureFormat::Astc8x5UnormSrgb,
        ktx2::Format::ASTC_8x6_UNORM_BLOCK => TextureFormat::Astc8x6Unorm,
        ktx2::Format::ASTC_8x6_SRGB_BLOCK => TextureFormat::Astc8x6UnormSrgb,
        ktx2::Format::ASTC_8x8_UNORM_BLOCK => TextureFormat::Astc8x8Unorm,
        ktx2::Format::ASTC_8x8_SRGB_BLOCK => TextureFormat::Astc8x8UnormSrgb,
        ktx2::Format::ASTC_10x5_UNORM_BLOCK => TextureFormat::Astc10x5Unorm,
        ktx2::Format::ASTC_10x5_SRGB_BLOCK => TextureFormat::Astc10x5UnormSrgb,
        ktx2::Format::ASTC_10x6_UNORM_BLOCK => TextureFormat::Astc10x6Unorm,
        ktx2::Format::ASTC_10x6_SRGB_BLOCK => TextureFormat::Astc10x6UnormSrgb,
        ktx2::Format::ASTC_10x8_UNORM_BLOCK => TextureFormat::Astc10x8Unorm,
        ktx2::Format::ASTC_10x8_SRGB_BLOCK => TextureFormat::Astc10x8UnormSrgb,
        ktx2::Format::ASTC_10x10_UNORM_BLOCK => TextureFormat::Astc10x10Unorm,
        ktx2::Format::ASTC_10x10_SRGB_BLOCK => TextureFormat::Astc10x10UnormSrgb,
        ktx2::Format::ASTC_12x10_UNORM_BLOCK => TextureFormat::Astc12x10Unorm,
        ktx2::Format::ASTC_12x10_SRGB_BLOCK => TextureFormat::Astc12x10UnormSrgb,
        ktx2::Format::ASTC_12x12_UNORM_BLOCK => TextureFormat::Astc12x12Unorm,
        ktx2::Format::ASTC_12x12_SRGB_BLOCK => TextureFormat::Astc12x12UnormSrgb,

        // ASTC HDR (SFLOAT) is not exposed in WebGPU
        ktx2::Format::ASTC_4x4_SFLOAT_BLOCK
        | ktx2::Format::ASTC_5x4_SFLOAT_BLOCK
        | ktx2::Format::ASTC_5x5_SFLOAT_BLOCK
        | ktx2::Format::ASTC_6x5_SFLOAT_BLOCK
        | ktx2::Format::ASTC_6x6_SFLOAT_BLOCK
        | ktx2::Format::ASTC_8x5_SFLOAT_BLOCK
        | ktx2::Format::ASTC_8x6_SFLOAT_BLOCK
        | ktx2::Format::ASTC_8x8_SFLOAT_BLOCK
        | ktx2::Format::ASTC_10x5_SFLOAT_BLOCK
        | ktx2::Format::ASTC_10x6_SFLOAT_BLOCK
        | ktx2::Format::ASTC_10x8_SFLOAT_BLOCK
        | ktx2::Format::ASTC_10x10_SFLOAT_BLOCK
        | ktx2::Format::ASTC_12x10_SFLOAT_BLOCK
        | ktx2::Format::ASTC_12x12_SFLOAT_BLOCK => return None,

        // Legacy packed formats (R4G4, 4444, 565, 5551, etc.) aren't available in WebGPU
        ktx2::Format::R4G4_UNORM_PACK8
        | ktx2::Format::R4G4B4A4_UNORM_PACK16
        | ktx2::Format::B4G4R4A4_UNORM_PACK16
        | ktx2::Format::R5G6B5_UNORM_PACK16
        | ktx2::Format::B5G6R5_UNORM_PACK16
        | ktx2::Format::R5G5B5A1_UNORM_PACK16
        | ktx2::Format::B5G5R5A1_UNORM_PACK16
        | ktx2::Format::A1R5G5B5_UNORM_PACK16 => return None,

        // Catch-all for unsupported formats
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_layout_bc7() {
        // 4x4 blocks, 16 bytes each.
        assert_eq!(
            block_dims(TextureFormat::Bc7RgbaUnormSrgb),
            Some((4, 4, 16))
        );
        assert!(is_block_compressed(TextureFormat::Bc7RgbaUnorm));
        // 1024 wide → 256 block columns × 16B = 4096B tight (already 256-aligned).
        assert_eq!(tight_bytes_per_row(TextureFormat::Bc7RgbaUnorm, 1024), 4096);
        assert_eq!(
            aligned_bytes_per_row(TextureFormat::Bc7RgbaUnorm, 1024),
            4096
        );
        // Non-multiple-of-4 width rounds up to whole blocks.
        assert_eq!(tight_bytes_per_row(TextureFormat::Bc7RgbaUnorm, 5), 2 * 16);
        // 1×1 mip is still one whole block.
        assert_eq!(mip_level_byte_size(TextureFormat::Bc7RgbaUnorm, 1, 1), 16);
        assert_eq!(rows_per_image(TextureFormat::Bc7RgbaUnorm, 256), 64);
    }

    #[test]
    fn block_layout_etc2_and_astc() {
        assert_eq!(block_dims(TextureFormat::Etc2Rgb8unorm), Some((4, 4, 8)));
        assert_eq!(
            block_dims(TextureFormat::Etc2Rgba8unormSrgb),
            Some((4, 4, 16))
        );
        assert_eq!(
            block_dims(TextureFormat::Astc4x4UnormSrgb),
            Some((4, 4, 16))
        );
        assert_eq!(
            block_dims(TextureFormat::Astc12x12Unorm),
            Some((12, 12, 16))
        );
        // 100 wide ASTC-12x12 → ceil(100/12)=9 blocks × 16B.
        assert_eq!(
            tight_bytes_per_row(TextureFormat::Astc12x12Unorm, 100),
            9 * 16
        );
        assert_eq!(rows_per_image(TextureFormat::Astc12x12Unorm, 100), 9);
    }

    #[test]
    fn uncompressed_layout() {
        assert!(!is_block_compressed(TextureFormat::Rgba8unormSrgb));
        assert_eq!(block_dims(TextureFormat::Rgba8unorm), None);
        assert_eq!(tight_bytes_per_row(TextureFormat::Rgba8unorm, 100), 400);
        assert_eq!(aligned_bytes_per_row(TextureFormat::Rgba8unorm, 100), 512);
        assert_eq!(rows_per_image(TextureFormat::Rgba8unorm, 37), 37);
        assert_eq!(
            mip_level_byte_size(TextureFormat::Rgba8unorm, 100, 10),
            4000
        );
    }

    #[cfg(feature = "ktx")]
    #[test]
    fn ktx_format_mapping_spot_checks() {
        assert_eq!(
            map_ktx_format(ktx2::Format::BC7_SRGB_BLOCK),
            Some(TextureFormat::Bc7RgbaUnormSrgb)
        );
        assert_eq!(
            map_ktx_format(ktx2::Format::ETC2_R8G8B8A8_UNORM_BLOCK),
            Some(TextureFormat::Etc2Rgba8unorm)
        );
        assert_eq!(
            map_ktx_format(ktx2::Format::ASTC_4x4_SRGB_BLOCK),
            Some(TextureFormat::Astc4x4UnormSrgb)
        );
        assert_eq!(
            map_ktx_format(ktx2::Format::R8G8B8A8_SRGB),
            Some(TextureFormat::Rgba8unormSrgb)
        );
        // RGB24 has no WebGPU equivalent.
        assert_eq!(map_ktx_format(ktx2::Format::R8G8B8_UNORM), None);
        // ASTC HDR is not in WebGPU.
        assert_eq!(map_ktx_format(ktx2::Format::ASTC_4x4_SFLOAT_BLOCK), None);
    }
}
