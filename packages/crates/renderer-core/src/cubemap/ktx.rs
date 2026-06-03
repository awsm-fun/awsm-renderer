//! KTX2 cubemap loading helpers.

use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::texture::TextureFormat;
use crate::{
    command::copy_texture::{Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo},
    error::{AwsmCoreError, Result},
    renderer::AwsmRendererWebGpu,
    texture::{Extent3d, TextureDescriptor, TextureDimension, TextureUsage},
};

/// Loads a KTX2 file from a URL.
pub async fn load_url(url: &str) -> anyhow::Result<ktx2::Reader<Vec<u8>>> {
    let resp: web_sys::Response = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| AwsmCoreError::Fetch(e.to_string()))?
        .into();

    let js_value = JsFuture::from(resp.array_buffer().map_err(AwsmCoreError::fetch)?)
        .await
        .map_err(AwsmCoreError::fetch)?;

    let array_buffer: ArrayBuffer = js_value.unchecked_into();

    let bytes = Uint8Array::new(&array_buffer).to_vec();

    Ok(ktx2::Reader::new(bytes).map_err(|e| AwsmCoreError::Ktx(e.to_string()))?)
}

/// Creates a cubemap texture from a KTX2 reader.
pub async fn create_texture(
    reader: &ktx2::Reader<Vec<u8>>,
    gpu: &AwsmRendererWebGpu,
) -> Result<(web_sys::GpuTexture, u32)> {
    let header = reader.header();

    if header.face_count != 6 {
        return Err(AwsmCoreError::Cubemap(
            "KTX file does not contain a cubemap".to_string(),
        ));
    }

    if header.layer_count != 0 {
        return Err(AwsmCoreError::Cubemap(
            "KTX file contains array textures, which are not supported for cubemaps".to_string(),
        ));
    }

    if header.pixel_depth > 1 {
        return Err(AwsmCoreError::Cubemap(
            "KTX file contains 3D textures, which are not supported for cubemaps".to_string(),
        ));
    }

    if header.supercompression_scheme.is_some() {
        return Err(AwsmCoreError::Cubemap(
            "KTX file uses supercompression, which is not supported".to_string(),
        ));
    }

    let ktx_format = match header.format {
        Some(f) => f,
        None => {
            return Err(AwsmCoreError::Cubemap(
                "KTX file does not specify a format".to_string(),
            ));
        }
    };

    let format = match map_ktx_format(ktx_format) {
        Some(format) => {
            // // Check for KTX metadata that might indicate exposure/scaling
            // for (key, value) in reader.key_value_data() {
            //     tracing::info!("metadata key: {key}");
            // }

            format
        }
        None => {
            return Err(AwsmCoreError::Cubemap(format!(
                "KTX file has unsupported format: {:?}",
                header.format
            )));
        }
    };

    // Warn about potential depth format compatibility issues
    if matches!(
        format,
        TextureFormat::Depth24plus | TextureFormat::Depth24plusStencil8
    ) {
        tracing::warn!("Using Depth24plus format - some backends implement this as 32-bit float internally. If texture upload fails, consider converting the asset to Depth32float format.");
    }

    // Validate device features for compressed formats
    if is_block_compressed(format) {
        // Note: In a full implementation, you would check gpu device features here
        // For now, we assume the features are available
        tracing::warn!(
            "Using compressed texture format {:?} - ensure device supports required features",
            format
        );
    }

    let descriptor = TextureDescriptor::new(
        format,
        Extent3d::new(header.pixel_width, Some(header.pixel_height), Some(6)),
        TextureUsage::new().with_texture_binding().with_copy_dst(),
    )
    .with_mip_level_count(header.level_count)
    .with_dimension(TextureDimension::N2d);

    let texture = gpu.create_texture(&descriptor.into())?;

    for (index, level) in reader.levels().enumerate() {
        // Calculate mip level dimensions with bounds checking
        let mip_width = if index < 32 {
            std::cmp::max(1u32, header.pixel_width >> index)
        } else {
            1u32
        };
        let mip_height = if index < 32 {
            std::cmp::max(1u32, header.pixel_height >> index)
        } else {
            1u32
        };

        // Validate level size matches expected tight size
        let rows = rows_per_image_units(format, mip_height);
        let tight_bpr = if let Some((bw, _bh, bpb)) = block_dims(format) {
            mip_width.div_ceil(bw) * bpb
        } else {
            mip_width * get_format_bytes_per_pixel(format)
        };
        let face_bytes_tight = tight_bpr as usize * rows as usize;
        let expected_level_len = face_bytes_tight * 6;

        if level.data.len() != expected_level_len {
            return Err(AwsmCoreError::Cubemap(format!(
                "Level {} byte length {} doesn't match expected face*rows*tight_bpr {} (possible KTX per-face padding not supported)",
                index, level.data.len(), expected_level_len
            )));
        }

        // Calculate values once per mip level
        let bpr = calculate_bytes_per_row(format, mip_width);
        let layout = TexelCopyBufferLayout::new()
            .with_bytes_per_row(bpr)
            .with_rows_per_image(rows);
        let size = Extent3d::new(mip_width, Some(mip_height), None);

        // Convert once for reuse
        let layout_ref = &layout.into();
        let size_ref = &size.into();

        for face in 0..6 {
            let destination = TexelCopyTextureInfo::new(&texture)
                .with_mip_level(index as u32)
                .with_origin(Origin3d::new().with_z(face as u32));

            // TODO: ideally fetch per-face slices from the KTX reader
            let face_data_tight =
                &level.data[face * face_bytes_tight..(face + 1) * face_bytes_tight];

            if bpr == tight_bpr {
                // No padding needed, use slice directly
                gpu.write_texture(&destination.into(), face_data_tight, layout_ref, size_ref)?;
            } else {
                // Need padding, create staging buffer
                let mut staging = vec![0u8; (bpr * rows) as usize];
                for r in 0..rows as usize {
                    let src = r * tight_bpr as usize..r * tight_bpr as usize + tight_bpr as usize;
                    let dst = r * bpr as usize..r * bpr as usize + tight_bpr as usize;
                    staging[dst].copy_from_slice(&face_data_tight[src]);
                }
                gpu.write_texture(
                    &destination.into(),
                    staging.as_slice(),
                    layout_ref,
                    size_ref,
                )?;
            }
        }
    }

    Ok((texture, header.level_count))
}

#[inline]
fn align_up(x: u32, align: u32) -> u32 {
    debug_assert!(align.is_power_of_two());
    (x + (align - 1)) & !(align - 1)
}

fn is_block_compressed(format: TextureFormat) -> bool {
    matches!(
        format,
        TextureFormat::Bc1RgbaUnorm
            | TextureFormat::Bc1RgbaUnormSrgb
            | TextureFormat::Bc2RgbaUnorm
            | TextureFormat::Bc2RgbaUnormSrgb
            | TextureFormat::Bc3RgbaUnorm
            | TextureFormat::Bc3RgbaUnormSrgb
            | TextureFormat::Bc4RUnorm
            | TextureFormat::Bc4RSnorm
            | TextureFormat::Bc5RgUnorm
            | TextureFormat::Bc5RgSnorm
            | TextureFormat::Bc6hRgbUfloat
            | TextureFormat::Bc6hRgbFloat
            | TextureFormat::Bc7RgbaUnorm
            | TextureFormat::Bc7RgbaUnormSrgb
            | TextureFormat::Etc2Rgb8unorm
            | TextureFormat::Etc2Rgb8unormSrgb
            | TextureFormat::Etc2Rgb8a1unorm
            | TextureFormat::Etc2Rgb8a1unormSrgb
            | TextureFormat::Etc2Rgba8unorm
            | TextureFormat::Etc2Rgba8unormSrgb
            | TextureFormat::EacR11unorm
            | TextureFormat::EacR11snorm
            | TextureFormat::EacRg11unorm
            | TextureFormat::EacRg11snorm
            | TextureFormat::Astc4x4Unorm
            | TextureFormat::Astc4x4UnormSrgb
            | TextureFormat::Astc5x4Unorm
            | TextureFormat::Astc5x4UnormSrgb
            | TextureFormat::Astc5x5Unorm
            | TextureFormat::Astc5x5UnormSrgb
            | TextureFormat::Astc6x5Unorm
            | TextureFormat::Astc6x5UnormSrgb
            | TextureFormat::Astc6x6Unorm
            | TextureFormat::Astc6x6UnormSrgb
            | TextureFormat::Astc8x5Unorm
            | TextureFormat::Astc8x5UnormSrgb
            | TextureFormat::Astc8x6Unorm
            | TextureFormat::Astc8x6UnormSrgb
            | TextureFormat::Astc8x8Unorm
            | TextureFormat::Astc8x8UnormSrgb
            | TextureFormat::Astc10x5Unorm
            | TextureFormat::Astc10x5UnormSrgb
            | TextureFormat::Astc10x6Unorm
            | TextureFormat::Astc10x6UnormSrgb
            | TextureFormat::Astc10x8Unorm
            | TextureFormat::Astc10x8UnormSrgb
            | TextureFormat::Astc10x10Unorm
            | TextureFormat::Astc10x10UnormSrgb
            | TextureFormat::Astc12x10Unorm
            | TextureFormat::Astc12x10UnormSrgb
            | TextureFormat::Astc12x12Unorm
            | TextureFormat::Astc12x12UnormSrgb
    )
}

fn block_dims(format: TextureFormat) -> Option<(u32, u32, u32)> {
    // (block_w, block_h, bytes_per_block)
    Some(match format {
        TextureFormat::Bc1RgbaUnorm
        | TextureFormat::Bc1RgbaUnormSrgb
        | TextureFormat::Bc4RUnorm
        | TextureFormat::Bc4RSnorm
        | TextureFormat::Etc2Rgb8unorm
        | TextureFormat::Etc2Rgb8unormSrgb
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
        | TextureFormat::Etc2Rgb8a1unorm
        | TextureFormat::Etc2Rgb8a1unormSrgb
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

fn calculate_bytes_per_row(format: TextureFormat, width: u32) -> u32 {
    if let Some((bw, _bh, bpb)) = block_dims(format) {
        // block columns * bytesPerBlock
        let blocks_x = width.div_ceil(bw);
        // 256 alignment required
        align_up(blocks_x * bpb, 256)
    } else {
        align_up(width * get_format_bytes_per_pixel(format), 256)
    }
}

fn rows_per_image_units(format: TextureFormat, height: u32) -> u32 {
    if let Some((_bw, bh, _bpb)) = block_dims(format) {
        height.div_ceil(bh) // block rows
    } else {
        height // pixel rows
    }
}

fn get_format_bytes_per_pixel(format: TextureFormat) -> u32 {
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

        // Block compressed formats - not used in this function since they're handled in calculate_bytes_per_row

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

fn map_ktx_format(format: ktx2::Format) -> Option<TextureFormat> {
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

        // Legacy packed formats (R4G4, 4444, 565, 5551, etc.) aren’t available in WebGPU
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
