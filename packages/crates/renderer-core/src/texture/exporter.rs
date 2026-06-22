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

    /// Read back an 8-bit texture (the swapchain / canvas formats) as tightly
    /// packed **display-space RGBA8** `(rgba, width, height)`. Like
    /// [`Self::export_texture_as_png`] but without the PNG encode — for callers
    /// that want raw pixels (luma stats, pixel probes, or their own encoder).
    ///
    /// The copy is submitted **before the first await**, so issuing this within a
    /// render frame (before the next `getCurrentTexture`/present) captures the
    /// just-rendered swapchain — the reliable replacement for `toDataURL`/
    /// `drawImage`, which return empty on a WebGPU canvas.
    pub async fn export_texture_as_rgba8(
        &self,
        texture: &web_sys::GpuTexture,
        width: u32,
        height: u32,
        array_index: u32,
        format: TextureFormat,
    ) -> Result<Vec<u8>> {
        let format_info = get_format_info(format, None)?;
        let unpadded_bytes_per_row = width * format_info.bytes_per_pixel;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
        let buffer_size = padded_bytes_per_row * height;

        let destination_buffer = self.create_buffer(
            &BufferDescriptor::new(
                Some("Texture RGBA Exporter"),
                buffer_size as usize,
                BufferUsage::new().with_copy_dst().with_map_read(),
            )
            .into(),
        )?;

        let command_encoder = self.create_command_encoder(Some("Texture RGBA Exporter"));
        let image_copy_texture =
            TexelCopyTextureInfo::new(texture).with_origin(Origin3d::new().with_z(array_index));
        let image_copy_buffer = TexelCopyBufferInfo::new(&destination_buffer)
            .with_bytes_per_row(padded_bytes_per_row)
            .with_rows_per_image(height);
        command_encoder.copy_texture_to_buffer(
            &image_copy_texture.into(),
            &image_copy_buffer.into(),
            &Extent3d::new(width, Some(height), Some(1)).into(),
        )?;
        // Submit BEFORE awaiting — this captures the current texture content.
        self.submit_commands(&command_encoder.finish());

        JsFuture::from(destination_buffer.map_async(MapMode::Read as u32))
            .await
            .map_err(AwsmCoreError::buffer_map)?;
        let array_buffer = destination_buffer
            .get_mapped_range()
            .map_err(AwsmCoreError::buffer_map_range)?;
        let padded_data: Vec<u8> = js_sys::Uint8Array::new(&array_buffer).to_vec();

        let mut data: Vec<u8> = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
        for row in 0..height {
            let row_start = (row * padded_bytes_per_row) as usize;
            data.extend_from_slice(
                &padded_data[row_start..row_start + unpadded_bytes_per_row as usize],
            );
        }
        destination_buffer.unmap();
        destination_buffer.destroy();

        let mut rgba = match format {
            TextureFormat::Rgba8unorm | TextureFormat::Rgba8unormSrgb => data,
            TextureFormat::Bgra8unorm | TextureFormat::Bgra8unormSrgb => {
                for chunk in data.chunks_exact_mut(4) {
                    chunk.swap(0, 2);
                }
                data
            }
            _ => return Err(AwsmCoreError::TextureExportUnsupportedPngEncoding(format)),
        };
        // Convert to display (sRGB-encoded) bytes when the source is linear, so
        // luma/pixel readers see what's on screen.
        if !format_info.is_srgb {
            rgba = convert_linear_to_srgb_u8(&rgba);
        }
        Ok(rgba)
    }

    /// Capture a swapchain/canvas texture as an **opaque** PNG.
    ///
    /// Builds on [`Self::export_texture_as_rgba8`] (BGRA→RGBA swizzle + sRGB
    /// display bytes handled there) then forces every pixel's alpha to fully
    /// opaque before encoding. A renderer presents to a canvas with a don't-care
    /// alpha channel — for an opaque-composited canvas the swapchain alpha is
    /// often left at 0, which would otherwise decode as a fully transparent PNG
    /// (RGB premultiplied away by image viewers / `drawImage`). A screenshot must
    /// be opaque, so we clamp alpha here. Used by `renderer.capture_frame()` (B2).
    pub async fn export_texture_as_png_opaque(
        &self,
        texture: &web_sys::GpuTexture,
        width: u32,
        height: u32,
        array_index: u32,
        format: TextureFormat,
    ) -> Result<Vec<u8>> {
        let mut rgba = self
            .export_texture_as_rgba8(texture, width, height, array_index, format)
            .await?;
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let mut png_output: Vec<u8> = Vec::new();
        PngEncoder::new(&mut png_output)
            .write_image(&rgba, width, height, ColorType::Rgba8.into())
            .map_err(AwsmCoreError::TextureExportFailedWrite)?;
        Ok(png_output)
    }

    /// **Synchronously** encode + submit a `copyTextureToBuffer` of `texture`
    /// into a fresh MAP_READ buffer, returning a [`TextureReadback`] handle whose
    /// async `finish_*` maps + decodes it later.
    ///
    /// Splitting the synchronous copy from the async readback is load-bearing for
    /// capturing a **worker `OffscreenCanvas` swapchain**: the canvas implicitly
    /// presents (and *expires* the current texture) when control returns to the
    /// event loop, so a deferred copy (e.g. one issued from a `spawn_local`
    /// microtask) can race past present and read a blank texture. Issuing the
    /// copy here, inside the render callback before yielding, snapshots the live
    /// frame; only the `mapAsync` readback is deferred. (`renderer.capture_frame`.)
    pub fn copy_texture_for_readback(
        &self,
        texture: &web_sys::GpuTexture,
        width: u32,
        height: u32,
        array_index: u32,
        format: TextureFormat,
    ) -> Result<TextureReadback> {
        let format_info = get_format_info(format, None)?;
        let unpadded_bytes_per_row = width * format_info.bytes_per_pixel;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
        let buffer_size = padded_bytes_per_row * height;

        let buffer = self.create_buffer(
            &BufferDescriptor::new(
                Some("Texture Readback"),
                buffer_size as usize,
                BufferUsage::new().with_copy_dst().with_map_read(),
            )
            .into(),
        )?;

        let command_encoder = self.create_command_encoder(Some("Texture Readback"));
        let image_copy_texture =
            TexelCopyTextureInfo::new(texture).with_origin(Origin3d::new().with_z(array_index));
        let image_copy_buffer = TexelCopyBufferInfo::new(&buffer)
            .with_bytes_per_row(padded_bytes_per_row)
            .with_rows_per_image(height);
        command_encoder.copy_texture_to_buffer(
            &image_copy_texture.into(),
            &image_copy_buffer.into(),
            &Extent3d::new(width, Some(height), Some(1)).into(),
        )?;
        self.submit_commands(&command_encoder.finish());

        Ok(TextureReadback {
            buffer,
            padded_bytes_per_row,
            unpadded_bytes_per_row,
            width,
            height,
            format,
            is_srgb: format_info.is_srgb,
        })
    }
}

/// A pending GPU→CPU texture readback. The `copyTextureToBuffer` was already
/// submitted (see [`AwsmRendererWebGpu::copy_texture_for_readback`]); the async
/// `finish_*` methods map the buffer and decode the bytes.
pub struct TextureReadback {
    buffer: web_sys::GpuBuffer,
    padded_bytes_per_row: u32,
    unpadded_bytes_per_row: u32,
    width: u32,
    height: u32,
    format: TextureFormat,
    is_srgb: bool,
}

impl TextureReadback {
    /// Map the buffer and return tightly-packed display-space RGBA8 bytes
    /// (BGRA→RGBA swizzle + linear→sRGB applied to match what's on screen).
    pub async fn finish_rgba8(self) -> Result<Vec<u8>> {
        JsFuture::from(self.buffer.map_async(MapMode::Read as u32))
            .await
            .map_err(AwsmCoreError::buffer_map)?;
        let array_buffer = self
            .buffer
            .get_mapped_range()
            .map_err(AwsmCoreError::buffer_map_range)?;
        let padded_data: Vec<u8> = js_sys::Uint8Array::new(&array_buffer).to_vec();

        let mut data: Vec<u8> =
            Vec::with_capacity((self.unpadded_bytes_per_row * self.height) as usize);
        for row in 0..self.height {
            let row_start = (row * self.padded_bytes_per_row) as usize;
            data.extend_from_slice(
                &padded_data[row_start..row_start + self.unpadded_bytes_per_row as usize],
            );
        }
        self.buffer.unmap();
        self.buffer.destroy();

        let mut rgba = match self.format {
            TextureFormat::Rgba8unorm | TextureFormat::Rgba8unormSrgb => data,
            TextureFormat::Bgra8unorm | TextureFormat::Bgra8unormSrgb => {
                for chunk in data.chunks_exact_mut(4) {
                    chunk.swap(0, 2);
                }
                data
            }
            _ => {
                return Err(AwsmCoreError::TextureExportUnsupportedPngEncoding(
                    self.format,
                ))
            }
        };
        if !self.is_srgb {
            rgba = convert_linear_to_srgb_u8(&rgba);
        }
        Ok(rgba)
    }

    /// Map + decode as an **opaque** PNG (alpha forced to 1.0). See
    /// [`AwsmRendererWebGpu::export_texture_as_png_opaque`] for why opaque.
    pub async fn finish_png_opaque(self) -> Result<Vec<u8>> {
        let width = self.width;
        let height = self.height;
        let mut rgba = self.finish_rgba8().await?;
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let mut png_output: Vec<u8> = Vec::new();
        PngEncoder::new(&mut png_output)
            .write_image(&rgba, width, height, ColorType::Rgba8.into())
            .map_err(AwsmCoreError::TextureExportFailedWrite)?;
        Ok(png_output)
    }
}
