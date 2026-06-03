//! Texture export helpers.

use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};
use wasm_bindgen_futures::JsFuture;

use crate::buffers::{BufferDescriptor, BufferUsage, MapMode};
use crate::command::copy_texture::{Origin3d, TexelCopyBufferInfo, TexelCopyTextureInfo};
use crate::error::{AwsmCoreError, Result};
use crate::texture::Extent3d;
use crate::{renderer::AwsmRendererWebGpu, texture::TextureFormat};

// Helper struct to hold parsed information about a GPU texture format.
#[derive(Debug, Clone)]
struct FormatInfo {
    bytes_per_pixel: u32,
    is_srgb: bool,
    // Add other fields as needed, e.g., channel_count, is_float, etc.
    // For this implementation, we only need bytes_per_pixel and is_srgb.
}

/// Analyzes a TextureFormat enum and returns a struct with its properties.
/// This helps in generalizing the buffer copy and data conversion logic.
fn get_format_info(format: TextureFormat, force_srgb: Option<bool>) -> Result<FormatInfo> {
    let mut info = match format {
        // 8-bit formats (1 byte per channel)
        TextureFormat::R8unorm
        | TextureFormat::R8snorm
        | TextureFormat::R8uint
        | TextureFormat::R8sint => Ok(FormatInfo {
            bytes_per_pixel: 1,
            is_srgb: false,
        }),

        // 16-bit formats (2 bytes per channel)
        TextureFormat::R16uint | TextureFormat::R16sint | TextureFormat::R16float => {
            Ok(FormatInfo {
                bytes_per_pixel: 2,
                is_srgb: false,
            })
        }
        TextureFormat::Rg8unorm
        | TextureFormat::Rg8snorm
        | TextureFormat::Rg8uint
        | TextureFormat::Rg8sint => Ok(FormatInfo {
            bytes_per_pixel: 2,
            is_srgb: false,
        }),

        // 32-bit formats (4 bytes per channel)
        TextureFormat::R32uint | TextureFormat::R32sint | TextureFormat::R32float => {
            Ok(FormatInfo {
                bytes_per_pixel: 4,
                is_srgb: false,
            })
        }
        TextureFormat::Rg16uint | TextureFormat::Rg16sint | TextureFormat::Rg16float => {
            Ok(FormatInfo {
                bytes_per_pixel: 4,
                is_srgb: false,
            })
        }
        TextureFormat::Rgba8unorm => Ok(FormatInfo {
            bytes_per_pixel: 4,
            is_srgb: false,
        }),
        TextureFormat::Rgba8unormSrgb => Ok(FormatInfo {
            bytes_per_pixel: 4,
            is_srgb: true,
        }),
        TextureFormat::Rgba8snorm | TextureFormat::Rgba8uint | TextureFormat::Rgba8sint => {
            Ok(FormatInfo {
                bytes_per_pixel: 4,
                is_srgb: false,
            })
        }
        TextureFormat::Bgra8unorm => Ok(FormatInfo {
            bytes_per_pixel: 4,
            is_srgb: false,
        }),
        TextureFormat::Bgra8unormSrgb => Ok(FormatInfo {
            bytes_per_pixel: 4,
            is_srgb: true,
        }),

        // 64-bit formats (8 bytes per channel)
        TextureFormat::Rg32uint | TextureFormat::Rg32sint | TextureFormat::Rg32float => {
            Ok(FormatInfo {
                bytes_per_pixel: 8,
                is_srgb: false,
            })
        }
        TextureFormat::Rgba16uint | TextureFormat::Rgba16sint | TextureFormat::Rgba16float => {
            Ok(FormatInfo {
                bytes_per_pixel: 8,
                is_srgb: false,
            })
        }

        // 128-bit formats (16 bytes per channel)
        TextureFormat::Rgba32uint | TextureFormat::Rgba32sint | TextureFormat::Rgba32float => {
            Ok(FormatInfo {
                bytes_per_pixel: 16,
                is_srgb: false,
            })
        }

        // Depth/stencil formats are not directly copyable in this way.
        _ => Err(AwsmCoreError::TextureExportUnsupportedFormat(format)),
    }?;

    if let Some(force_srgb) = force_srgb {
        info.is_srgb = force_srgb;
    }

    Ok(info)
}

fn linear_to_srgb(linear: f32) -> f32 {
    if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

fn convert_linear_to_srgb_u8(data: &[u8]) -> Vec<u8> {
    data.chunks_exact(4)
        .flat_map(|pixel| {
            let r = linear_to_srgb(pixel[0] as f32 / 255.0);
            let g = linear_to_srgb(pixel[1] as f32 / 255.0);
            let b = linear_to_srgb(pixel[2] as f32 / 255.0);
            let a = pixel[3]; // Alpha is not gamma corrected

            vec![(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, a]
        })
        .collect()
}

/// Main function to export a GpuTexture to a PNG byte vector.
/// It handles copying the texture to a buffer, reading it back to the CPU,
/// and encoding it. Now supports texture arrays via the `array_index` parameter.
impl AwsmRendererWebGpu {
    #[allow(clippy::too_many_arguments)]
    pub async fn export_texture_as_png(
        &self,
        texture: &web_sys::GpuTexture,
        mut width: u32,
        mut height: u32,
        array_index: u32,
        format: TextureFormat,
        mipmap_level: Option<u32>,
        use_16bit_png: bool,
        force_srgb: Option<bool>, // typically Some(true) since that's what PNG expects
    ) -> Result<Vec<u8>> {
        // adjust for mipmap
        if let Some(mipmap_level) = mipmap_level {
            width = (width >> mipmap_level).max(1);
            height = (height >> mipmap_level).max(1);
        }

        // 1. Get format information to determine buffer size and processing steps.
        let format_info = get_format_info(format, force_srgb)?;

        // 2. Create a destination buffer on the GPU to copy the texture data into.
        // The buffer must have MAP_READ usage to allow reading its data on the CPU.
        // WebGPU requires bytes_per_row to be a multiple of 256 for copy_texture_to_buffer
        let unpadded_bytes_per_row = width * format_info.bytes_per_pixel;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
        let buffer_size = padded_bytes_per_row * height;

        let buffer_descriptor = BufferDescriptor::new(
            Some("Texture Exporter"),
            buffer_size as usize,
            BufferUsage::new().with_copy_dst().with_map_read(),
        );
        let destination_buffer = self.create_buffer(&buffer_descriptor.into())?;

        // 3. Create a command encoder and issue the copy command.
        let command_encoder = self.create_command_encoder(Some("Texture Exporter"));

        let mut image_copy_texture = TexelCopyTextureInfo::new(texture).with_origin(
            Origin3d::new().with_z(array_index), // Specify the array index for texture array
        );

        if let Some(mipmap_level) = mipmap_level {
            image_copy_texture = image_copy_texture.with_mip_level(mipmap_level);
        }

        let image_copy_buffer = TexelCopyBufferInfo::new(&destination_buffer)
            .with_bytes_per_row(padded_bytes_per_row)
            .with_rows_per_image(height);

        // always copying a single layer
        let extent = Extent3d::new(width, Some(height), Some(1));

        command_encoder.copy_texture_to_buffer(
            &image_copy_texture.into(),
            &image_copy_buffer.into(),
            &extent.into(),
        )?;

        // 4. Submit the command to the GPU queue.
        self.submit_commands(&command_encoder.finish());

        // 5. Map the buffer to read its contents from the CPU.
        // This is an async operation, so we await the promise.
        let buffer_slice_promise = destination_buffer.map_async(MapMode::Read as u32);
        JsFuture::from(buffer_slice_promise)
            .await
            .map_err(AwsmCoreError::buffer_map)?;

        // 6. Get the mapped data as an ArrayBuffer and copy it into a Rust Vec.
        let array_buffer = destination_buffer
            .get_mapped_range()
            .map_err(AwsmCoreError::buffer_map_range)?;
        let padded_data: Vec<u8> = js_sys::Uint8Array::new(&array_buffer).to_vec();

        // Remove padding from each row to get the actual texture data
        let mut data: Vec<u8> = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
        for row in 0..height {
            let row_start = (row * padded_bytes_per_row) as usize;
            let row_end = row_start + unpadded_bytes_per_row as usize;
            data.extend_from_slice(&padded_data[row_start..row_end]);
        }

        // It's important to unmap the buffer once we're done with the data.
        destination_buffer.unmap();

        // 7. Process the raw buffer data and encode it as a PNG.
        let mut png_output: Vec<u8> = Vec::new();
        let color_type: ColorType;

        // The PNG encoder needs a byte slice. We prepare a new Vec to hold the final, correctly formatted data.
        let final_pixel_data: Vec<u8> = match format {
            // For standard 8-bit RGBA, handle sRGB conversion if needed.
            TextureFormat::Rgba8unorm | TextureFormat::Rgba8unormSrgb => {
                color_type = ColorType::Rgba8;
                if format_info.is_srgb {
                    // Data is already in sRGB space, no conversion needed
                    data
                } else {
                    // Data is linear, convert to sRGB for PNG
                    convert_linear_to_srgb_u8(&data)
                }
            }
            // For BGRA, we need to swap the R and B channels to get RGBA, then handle sRGB.
            TextureFormat::Bgra8unorm | TextureFormat::Bgra8unormSrgb => {
                color_type = ColorType::Rgba8;
                // Swap B and R channels
                for chunk in data.chunks_exact_mut(4) {
                    chunk.swap(0, 2);
                }
                if format_info.is_srgb {
                    data
                } else {
                    convert_linear_to_srgb_u8(&data)
                }
            }
            // For 16-bit float, we need to convert f16 to u16 for the PNG encoder.
            TextureFormat::Rgba16float if use_16bit_png => {
                color_type = ColorType::Rgba16;
                let float_data: Vec<half::f16> = data
                    .chunks_exact(2)
                    .map(|chunk| half::f16::from_le_bytes(chunk.try_into().unwrap()))
                    .collect();

                let u16_data: Vec<u16> = if format_info.is_srgb {
                    // Unlikely case for f16, but handle it
                    float_data
                        .into_iter()
                        .map(|f| (f.to_f32().clamp(0.0, 1.0) * 65535.0) as u16)
                        .collect()
                } else {
                    // Convert linear to sRGB, then to u16
                    float_data
                        .into_iter()
                        .map(|f| {
                            let linear = f.to_f32().clamp(0.0, 1.0);
                            let srgb = linear_to_srgb(linear);
                            (srgb * 65535.0) as u16
                        })
                        .collect()
                };

                // The image crate expects a &[u8], so we must cast our &[u16].
                // This is safe because we're just viewing the same memory as bytes.
                unsafe {
                    std::slice::from_raw_parts(
                        u16_data.as_ptr() as *const u8,
                        u16_data.len() * std::mem::size_of::<u16>(),
                    )
                }
                .to_vec()
            }
            // Fallback for 16-bit float to 8-bit PNG (loses precision).
            TextureFormat::Rgba16float => {
                color_type = ColorType::Rgba8;
                let float_data: Vec<half::f16> = data
                    .chunks_exact(2)
                    .map(|chunk| half::f16::from_le_bytes(chunk.try_into().unwrap()))
                    .collect();

                if format_info.is_srgb {
                    float_data
                        .into_iter()
                        .map(|f| (f.to_f32().clamp(0.0, 1.0) * 255.0) as u8)
                        .collect()
                } else {
                    float_data
                        .into_iter()
                        .map(|f| {
                            let linear = f.to_f32().clamp(0.0, 1.0);
                            let srgb = linear_to_srgb(linear);
                            (srgb * 255.0) as u8
                        })
                        .collect()
                }
            }
            // Add other format handlers here as needed.
            _ => {
                destination_buffer.destroy(); // Clean up before erroring
                return Err(AwsmCoreError::TextureExportUnsupportedPngEncoding(format));
            }
        };

        // 8. Use the image crate to write the PNG data.
        let encoder = PngEncoder::new(&mut png_output);
        encoder
            .write_image(&final_pixel_data, width, height, color_type.into())
            .map_err(AwsmCoreError::TextureExportFailedWrite)?;

        // 9. IMPORTANT: Clean up the GPU buffer to prevent memory leaks.
        destination_buffer.destroy();

        Ok(png_output)
    }
}
