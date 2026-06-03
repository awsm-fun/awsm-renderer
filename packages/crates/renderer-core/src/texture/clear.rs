//! Helpers for clearing textures via buffer copies.

use crate::buffers::{BufferDescriptor, BufferUsage};
use crate::command::copy_texture::{Origin3d, TexelCopyBufferInfo, TexelCopyTextureInfo};
use crate::error::{AwsmCoreError, Result};
use crate::texture::Extent3d;
use crate::{renderer::AwsmRendererWebGpu, texture::TextureFormat};

/// Utility for clearing textures in chunks with a staging buffer.
pub struct TextureClearer {
    buffer: web_sys::GpuBuffer,
    width: u32,
    height: u32,
    aligned_row_bytes: u32,
    chunk_height: u32,
    chunks: u32,
}

impl TextureClearer {
    /// Creates a texture clearer for a specific format and size.
    pub fn new(
        gpu: &AwsmRendererWebGpu,
        format: TextureFormat,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let bytes_per_pixel = match format {
            TextureFormat::Rgba16float => 8,
            TextureFormat::R32float => 4,
            TextureFormat::Rgba8unorm => 4,
            TextureFormat::Depth24plus => 4,
            _ => return Err(AwsmCoreError::TextureClearUnsupportedFormat(format)),
        };

        let row_bytes = width * bytes_per_pixel;
        let aligned_row_bytes = row_bytes.next_multiple_of(256);

        // Decide the chunk height
        let max_buf = (gpu.device.limits().max_buffer_size() as u64 * 9) / 10;
        let chunk_height = ((max_buf / aligned_row_bytes as u64) as u32)
            .min(height)
            .max(1);

        let chunks = height.div_ceil(chunk_height);

        let buffer_size = (aligned_row_bytes * chunk_height) as usize;
        let buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Texture Clearer"),
                buffer_size,
                BufferUsage::new().with_copy_src().with_copy_dst(),
            )
            .into(),
        )?;

        // Clear buffer to zero once
        let encoder = gpu.create_command_encoder(Some("Texture Clearer"));
        encoder.clear_buffer(&buffer, None, None);
        gpu.submit_commands(&encoder.finish());

        Ok(Self {
            buffer,
            width,
            height,
            aligned_row_bytes,
            chunk_height,
            chunks,
        })
    }

    /// Clears the target texture to zero.
    pub fn clear(&self, gpu: &AwsmRendererWebGpu, texture: &web_sys::GpuTexture) -> Result<()> {
        let encoder = gpu.create_command_encoder(Some("Texture Clearer"));

        for i in 0..self.chunks {
            let y = i * self.chunk_height;
            let h = (self.height - y).min(self.chunk_height);

            encoder.copy_buffer_to_texture(
                &TexelCopyBufferInfo {
                    buffer: &self.buffer,
                    offset: None,
                    bytes_per_row: Some(self.aligned_row_bytes),
                    rows_per_image: Some(h),
                }
                .into(),
                &TexelCopyTextureInfo {
                    texture,
                    aspect: None,
                    mip_level: None,
                    origin: Some(Origin3d::new().with_y(y)),
                }
                .into(),
                &Extent3d {
                    width: self.width,
                    height: Some(h),
                    depth_or_array_layers: Some(1),
                }
                .into(),
            )?;
        }

        gpu.submit_commands(&encoder.finish());
        Ok(())
    }
}
