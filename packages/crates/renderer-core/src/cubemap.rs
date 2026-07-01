//! Cubemap image loading and texture creation helpers.

cfg_if::cfg_if! {
    if #[cfg(feature = "ktx")] {
        pub mod ktx;
        use std::sync::Arc;
    }
}

pub mod images;

use crate::{
    command::copy_texture::{Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo},
    cubemap::images::{CubemapBitmapColors, CubemapSkyGradient},
    error::{AwsmCoreError, Result},
    image::ImageData,
    renderer::AwsmRendererWebGpu,
    texture::{
        mipmap::{generate_mipmaps, MipmapTextureKind},
        Extent3d, TextureViewDescriptor, TextureViewDimension,
    },
};

/// Cubemap face index, mapped to texture array layers.
///
/// Layer order matches WebGPU cubemap conventions:
/// +X, -X, +Y, -Y, +Z, -Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum CubemapFace {
    XPositive = 0,
    XNegative = 1,
    YPositive = 2,
    YNegative = 3,
    ZPositive = 4,
    ZNegative = 5,
}

impl CubemapFace {
    /// All cubemap faces in upload order.
    pub const ALL: [Self; 6] = [
        Self::XPositive,
        Self::XNegative,
        Self::YPositive,
        Self::YNegative,
        Self::ZPositive,
        Self::ZNegative,
    ];

    /// Returns the texture array layer index for this face.
    pub fn layer_index(self) -> u32 {
        self as u32
    }
}

/// Raw byte layout for cubemap texture uploads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CubemapBytesLayout {
    pub bytes_per_row: u32,
    pub rows_per_image: u32,
    pub offset: u64,
}

impl CubemapBytesLayout {
    /// Creates a layout with zero offset.
    pub fn new(bytes_per_row: u32, rows_per_image: u32) -> Self {
        Self {
            bytes_per_row,
            rows_per_image,
            offset: 0,
        }
    }

    /// Sets the byte offset into the upload buffer.
    pub fn with_offset(mut self, offset: u64) -> Self {
        self.offset = offset;
        self
    }

    fn into_texel_layout(self) -> TexelCopyBufferLayout {
        TexelCopyBufferLayout::new()
            .with_bytes_per_row(self.bytes_per_row)
            .with_rows_per_image(self.rows_per_image)
            .with_offset(self.offset)
    }
}

/// Source cubemap data backed by KTX or six images.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum CubemapImage {
    #[cfg(feature = "ktx")]
    /// KTX2 cubemap source.
    Ktx(Arc<ktx2::Reader<Vec<u8>>>),
    /// Individual images for each cubemap face.
    Images {
        z_positive: ImageData,
        z_negative: ImageData,
        x_positive: ImageData,
        x_negative: ImageData,
        y_positive: ImageData,
        y_negative: ImageData,
        mipmaps: bool,
    },
}

impl CubemapImage {
    cfg_if::cfg_if! {
        if #[cfg(feature = "ktx")] {
            /// Loads a KTX2 cubemap from a URL.
            pub async fn load_url_ktx(url:&str) -> anyhow::Result<Self> {
                let reader = ktx::load_url(url).await?;

                Ok(CubemapImage::Ktx(Arc::new(reader)))
            }

            // returns mip count as well
            /// Creates a GPU cubemap texture and view.
            pub async fn create_texture_and_view(
                &self,
                gpu: &AwsmRendererWebGpu,
                label: Option<&str>,
            ) -> Result<(web_sys::GpuTexture, web_sys::GpuTextureView, u32)> {
                let (texture, mip_count) = match self {
                    CubemapImage::Ktx(reader) => {
                        ktx::create_texture(reader, gpu).await
                    },
                    CubemapImage::Images { z_positive, z_negative, x_positive, x_negative, y_positive, y_negative, mipmaps } => {
                        images::create_texture(gpu, z_positive, z_negative, x_positive, x_negative, y_positive, y_negative, *mipmaps).await
                    }
                }?;


                let view = create_texture_view(&texture, label)?;

                Ok((texture, view, mip_count))

            }

            /// Creates a cubemap from solid colors for each face.
            pub async fn new_colors(colors: CubemapBitmapColors, width: u32, height: u32) -> Result<Self> {
                images::new_colors(colors, width, height).await
            }

            /// Creates a cubemap from a simple sky gradient.
            pub async fn new_sky_gradient(colors: CubemapSkyGradient, width: u32, height: u32) -> Result<Self> {
                images::new_sky_gradient(colors, width, height).await
            }
        } else {
            /// Creates a GPU cubemap texture and view.
            pub async fn create_texture_and_view(
                &self,
                gpu: &AwsmRendererWebGpu,
                label: Option<&str>,
            ) -> Result<(web_sys::GpuTexture, web_sys::GpuTextureView, u32)> {
                let (texture, mip_count) = match self {
                    CubemapImage::Images { z_positive, z_negative, x_positive, x_negative, y_positive, y_negative, mipmaps } => {
                        images::create_texture(gpu, z_positive, z_negative, x_positive, x_negative, y_positive, y_negative, *mipmaps).await
                    }
                }?;

                let view = create_texture_view(&texture, label)?;
                Ok((texture, view, mip_count))

            }

            /// Creates a cubemap from solid colors for each face.
            pub async fn new_colors(colors: CubemapBitmapColors, width: u32, height: u32) -> Result<Self> {
                images::new_colors(colors, width, height).await
            }

            /// Creates a cubemap from a simple sky gradient.
            pub async fn new_sky_gradient(colors: CubemapSkyGradient, width: u32, height: u32) -> Result<Self> {
                images::new_sky_gradient(colors, width, height).await
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// Updates one cubemap face in-place from raw bytes.
pub fn update_texture_face(
    gpu: &AwsmRendererWebGpu,
    texture: &web_sys::GpuTexture,
    face: CubemapFace,
    mip_level: u32,
    width: u32,
    height: u32,
    data: &[u8],
    layout: CubemapBytesLayout,
) -> Result<()> {
    validate_dimensions(width, height)?;
    validate_layout(data, layout, 1)?;

    let destination = TexelCopyTextureInfo::new(texture)
        .with_mip_level(mip_level)
        .with_origin(Origin3d::new().with_z(face.layer_index()));

    let size = Extent3d::new(width, Some(height), Some(1));
    let layout = layout.into_texel_layout();

    gpu.write_texture(&destination.into(), data, &layout.into(), &size.into())
}

/// Updates all six cubemap faces in-place from one contiguous byte buffer.
///
/// Data must be packed in face order: +X, -X, +Y, -Y, +Z, -Z.
pub fn update_texture_all_faces(
    gpu: &AwsmRendererWebGpu,
    texture: &web_sys::GpuTexture,
    mip_level: u32,
    width: u32,
    height: u32,
    data: &[u8],
    layout: CubemapBytesLayout,
) -> Result<()> {
    validate_dimensions(width, height)?;
    validate_layout(data, layout, 6)?;

    let destination = TexelCopyTextureInfo::new(texture)
        .with_mip_level(mip_level)
        .with_origin(Origin3d::new().with_z(0));

    let size = Extent3d::new(width, Some(height), Some(6));
    let layout = layout.into_texel_layout();

    gpu.write_texture(&destination.into(), data, &layout.into(), &size.into())
}

/// Regenerates all cubemap mip levels from mip level 0.
pub async fn regenerate_texture_mipmaps(
    gpu: &AwsmRendererWebGpu,
    texture: &web_sys::GpuTexture,
    mip_levels: u32,
) -> Result<()> {
    generate_mipmaps(
        gpu,
        texture,
        &[
            MipmapTextureKind::Albedo,
            MipmapTextureKind::Albedo,
            MipmapTextureKind::Albedo,
            MipmapTextureKind::Albedo,
            MipmapTextureKind::Albedo,
            MipmapTextureKind::Albedo,
        ],
        mip_levels,
    )
    .await
}

fn create_texture_view(
    texture: &web_sys::GpuTexture,
    label: Option<&str>,
) -> Result<web_sys::GpuTextureView> {
    texture
        .create_view_with_descriptor(
            &TextureViewDescriptor::new(label)
                .with_dimension(TextureViewDimension::Cube)
                .into(),
        )
        .map_err(AwsmCoreError::create_texture_view)
}

fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(AwsmCoreError::Cubemap(
            "Cubemap update dimensions must be non-zero".to_string(),
        ));
    }

    if width != height {
        return Err(AwsmCoreError::Cubemap(format!(
            "Cubemap faces must be square, got {}x{}",
            width, height
        )));
    }

    Ok(())
}

fn validate_layout(data: &[u8], layout: CubemapBytesLayout, layer_count: u64) -> Result<()> {
    if layout.bytes_per_row == 0 {
        return Err(AwsmCoreError::Cubemap(
            "Cubemap update bytes_per_row must be non-zero".to_string(),
        ));
    }

    if layout.rows_per_image == 0 {
        return Err(AwsmCoreError::Cubemap(
            "Cubemap update rows_per_image must be non-zero".to_string(),
        ));
    }

    let per_layer = (layout.bytes_per_row as u64)
        .checked_mul(layout.rows_per_image as u64)
        .ok_or_else(|| {
            AwsmCoreError::Cubemap(
                "Cubemap update layout overflow while calculating layer byte size".to_string(),
            )
        })?;

    let total_bytes = per_layer.checked_mul(layer_count).ok_or_else(|| {
        AwsmCoreError::Cubemap(
            "Cubemap update layout overflow while calculating total byte size".to_string(),
        )
    })?;

    let required = layout.offset.checked_add(total_bytes).ok_or_else(|| {
        AwsmCoreError::Cubemap(
            "Cubemap update layout overflow while applying data offset".to_string(),
        )
    })?;

    if (data.len() as u64) < required {
        return Err(AwsmCoreError::Cubemap(format!(
            "Cubemap update buffer is too small: need at least {required} bytes, got {}",
            data.len()
        )));
    }

    Ok(())
}
