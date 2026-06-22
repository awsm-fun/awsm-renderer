//! Helpers for generating cubemap images.

use crate::command::color::Color;
use crate::cubemap::CubemapImage;
use crate::image::bitmap::{create_color, create_vertical_gradient};
use crate::image::ImageData;
use crate::texture::mipmap::{calculate_mipmap_levels, generate_mipmaps, MipmapTextureKind};
use crate::{
    command::copy_texture::Origin3d,
    error::{AwsmCoreError, Result},
    renderer::AwsmRendererWebGpu,
    texture::{Extent3d, TextureDescriptor, TextureDimension, TextureUsage},
};

/// Solid colors for each cubemap face.
#[derive(Clone, Debug)]
pub struct CubemapBitmapColors {
    pub z_positive: Color,
    pub z_negative: Color,
    pub x_positive: Color,
    pub x_negative: Color,
    pub y_positive: Color,
    pub y_negative: Color,
}

impl CubemapBitmapColors {
    /// Uses the same color for all faces.
    pub fn all(color: Color) -> Self {
        Self {
            z_positive: color.clone(),
            z_negative: color.clone(),
            x_positive: color.clone(),
            x_negative: color.clone(),
            y_positive: color.clone(),
            y_negative: color.clone(),
        }
    }
}

/// Gradient colors for a sky-like cubemap.
#[derive(Clone, Debug)]
pub struct CubemapSkyGradient {
    pub zenith: Color,
    pub nadir: Color,
}

impl CubemapSkyGradient {
    /// Creates a sky gradient from zenith and nadir colors.
    pub fn new(zenith: Color, nadir: Color) -> Self {
        Self { zenith, nadir }
    }
}

impl Default for CubemapSkyGradient {
    fn default() -> Self {
        Self {
            zenith: Color::new_values(0.4, 0.65, 1.0, 1.0),
            nadir: Color::new_values(0.55, 0.45, 0.35, 1.0),
        }
    }
}

/// Creates a cubemap with solid color faces.
pub async fn new_colors(
    colors: CubemapBitmapColors,
    width: u32,
    height: u32,
) -> Result<CubemapImage> {
    let z_positive = create_color(colors.z_positive, width, height, None).await?;
    let z_negative = create_color(colors.z_negative, width, height, None).await?;
    let x_positive = create_color(colors.x_positive, width, height, None).await?;
    let x_negative = create_color(colors.x_negative, width, height, None).await?;
    let y_positive = create_color(colors.y_positive, width, height, None).await?;
    let y_negative = create_color(colors.y_negative, width, height, None).await?;

    Ok(CubemapImage::Images {
        z_positive: ImageData::Bitmap {
            image: z_positive,
            options: None,
        },

        z_negative: ImageData::Bitmap {
            image: z_negative,
            options: None,
        },

        x_positive: ImageData::Bitmap {
            image: x_positive,
            options: None,
        },

        x_negative: ImageData::Bitmap {
            image: x_negative,
            options: None,
        },

        y_positive: ImageData::Bitmap {
            image: y_positive,
            options: None,
        },

        y_negative: ImageData::Bitmap {
            image: y_negative,
            options: None,
        },

        mipmaps: true,
    })
}

/// Creates a cubemap with a vertical sky gradient on side faces.
pub async fn new_sky_gradient(
    colors: CubemapSkyGradient,
    width: u32,
    height: u32,
) -> Result<CubemapImage> {
    let zenith_color = colors.zenith.clone();
    let nadir_color = colors.nadir.clone();

    let x_positive = create_vertical_gradient(
        zenith_color.clone(),
        nadir_color.clone(),
        width,
        height,
        None,
    )
    .await?;
    let x_negative = create_vertical_gradient(
        zenith_color.clone(),
        nadir_color.clone(),
        width,
        height,
        None,
    )
    .await?;
    let z_positive = create_vertical_gradient(
        zenith_color.clone(),
        nadir_color.clone(),
        width,
        height,
        None,
    )
    .await?;
    let z_negative = create_vertical_gradient(
        zenith_color.clone(),
        nadir_color.clone(),
        width,
        height,
        None,
    )
    .await?;

    let y_positive = create_color(zenith_color, width, height, None).await?;
    let y_negative = create_color(nadir_color, width, height, None).await?;

    Ok(CubemapImage::Images {
        z_positive: ImageData::Bitmap {
            image: z_positive,
            options: None,
        },

        z_negative: ImageData::Bitmap {
            image: z_negative,
            options: None,
        },

        x_positive: ImageData::Bitmap {
            image: x_positive,
            options: None,
        },

        x_negative: ImageData::Bitmap {
            image: x_negative,
            options: None,
        },

        y_positive: ImageData::Bitmap {
            image: y_positive,
            options: None,
        },

        y_negative: ImageData::Bitmap {
            image: y_negative,
            options: None,
        },

        mipmaps: true,
    })
}

/// Project an equirectangular (lat/long) RGBA8 panorama into a cubemap with
/// `face_size`² faces (§18). For each face texel the cube direction is sampled
/// from the equirect (bilinear, longitude wrapping). Pure-CPU, so an
/// agent-authored panorama (from `create_texture`) becomes a skybox / IBL
/// source. Call at a large `face_size` for the skybox + specular env, and a tiny
/// one for a cheap diffuse-irradiance approximation (the heavy box-downsample
/// stands in for a true cosine convolution).
pub async fn new_equirect(
    rgba: &[u8],
    src_w: u32,
    src_h: u32,
    face_size: u32,
) -> Result<CubemapImage> {
    let faces = project_equirect_faces(rgba, src_w, src_h, face_size);
    let mut bitmaps = Vec::with_capacity(6);
    for face in &faces {
        let bitmap =
            crate::image::bitmap::create_from_rgba(face, face_size, face_size, None).await?;
        bitmaps.push(ImageData::Bitmap {
            image: bitmap,
            options: None,
        });
    }
    // `faces`/`bitmaps` are ordered +X, -X, +Y, -Y, +Z, -Z.
    let mut it = bitmaps.into_iter();
    Ok(CubemapImage::Images {
        x_positive: it.next().unwrap(),
        x_negative: it.next().unwrap(),
        y_positive: it.next().unwrap(),
        y_negative: it.next().unwrap(),
        z_positive: it.next().unwrap(),
        z_negative: it.next().unwrap(),
        mipmaps: true,
    })
}

/// Sample an RGBA8 image with bilinear filtering at normalized `(u, v)` (origin
/// top-left). Longitude `u` wraps; latitude `v` clamps (poles).
fn equirect_bilinear(rgba: &[u8], w: u32, h: u32, u: f32, v: f32) -> [u8; 4] {
    let uw = u - u.floor(); // wrap into [0, 1)
    let fx = uw * w as f32 - 0.5;
    let fy = (v * h as f32 - 0.5).clamp(0.0, h as f32 - 1.0);
    let x0 = fx.floor().rem_euclid(w as f32) as u32;
    let x1 = (x0 + 1) % w;
    let y0 = fy.floor() as u32;
    let y1 = (y0 + 1).min(h - 1);
    let tx = fx - fx.floor();
    let ty = fy - fy.floor();
    let px = |x: u32, y: u32| {
        let o = ((y * w + x) * 4) as usize;
        [rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]]
    };
    let lerp = |a: u8, b: u8, t: f32| (a as f32 * (1.0 - t) + b as f32 * t).round() as u8;
    let (c00, c10, c01, c11) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    let mut out = [0u8; 4];
    for k in 0..4 {
        out[k] = lerp(lerp(c00[k], c10[k], tx), lerp(c01[k], c11[k], tx), ty);
    }
    out
}

/// CPU equirect→cubemap face projection. Returns six `face_size`² RGBA8 buffers
/// in the order +X, -X, +Y, -Y, +Z, -Z. Pure (no GPU/web) — unit-testable.
fn project_equirect_faces(rgba: &[u8], src_w: u32, src_h: u32, n: u32) -> [Vec<u8>; 6] {
    use std::f32::consts::PI;
    std::array::from_fn(|face| {
        let mut buf = vec![0u8; (n * n * 4) as usize];
        for j in 0..n {
            let t = 2.0 * (j as f32 + 0.5) / n as f32 - 1.0;
            for i in 0..n {
                let s = 2.0 * (i as f32 + 0.5) / n as f32 - 1.0;
                // Standard GL cube-face → direction.
                let d = match face {
                    0 => [1.0, -t, -s],
                    1 => [-1.0, -t, s],
                    2 => [s, 1.0, t],
                    3 => [s, -1.0, -t],
                    4 => [s, -t, 1.0],
                    _ => [-s, -t, -1.0],
                };
                let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
                let (x, y, z) = (d[0] / len, d[1] / len, d[2] / len);
                let u = z.atan2(x) / (2.0 * PI) + 0.5;
                let v = 0.5 - y.clamp(-1.0, 1.0).asin() / PI;
                let px = equirect_bilinear(rgba, src_w, src_h, u, v);
                let o = ((j * n + i) * 4) as usize;
                buf[o..o + 4].copy_from_slice(&px);
            }
        }
        buf
    })
}

#[allow(clippy::too_many_arguments)]
/// Creates a cubemap texture from six images.
pub async fn create_texture(
    gpu: &AwsmRendererWebGpu,
    z_positive: &ImageData,
    z_negative: &ImageData,
    x_positive: &ImageData,
    x_negative: &ImageData,
    y_positive: &ImageData,
    y_negative: &ImageData,
    generate_mipmap: bool,
) -> Result<(web_sys::GpuTexture, u32)> {
    // Collect all faces in the correct order (required for cubemaps)
    let faces = [
        &x_positive, // +X
        &x_negative, // -X
        &y_positive, // +Y
        &y_negative, // -Y
        &z_positive, // +Z
        &z_negative, // -Z
    ];

    // Validate all faces have the same size and format
    let (width, height) = faces[0].size();
    let format = faces[0].format();

    for (i, face) in faces.iter().enumerate() {
        let (face_width, face_height) = face.size();
        if face_width != width || face_height != height {
            return Err(AwsmCoreError::Cubemap(format!(
                "Face {} size ({}, {}) doesn't match first face size ({}, {})",
                i, face_width, face_height, width, height
            )));
        }
        if face.format() != format {
            return Err(AwsmCoreError::Cubemap(format!(
                "Face {} format {:?} doesn't match first face format {:?}",
                i,
                face.format(),
                format
            )));
        }
    }

    // Ensure the texture is square (cubemap requirement)
    if width != height {
        return Err(AwsmCoreError::Cubemap(format!(
            "Cubemap faces must be square, got {}x{}",
            width, height
        )));
    }

    // Calculate mipmap levels if needed
    let mut usage = TextureUsage::new()
        .with_texture_binding()
        .with_render_attachment()
        .with_copy_dst();

    if generate_mipmap {
        usage = usage.with_storage_binding();
    }

    let mipmap_levels = if generate_mipmap {
        calculate_mipmap_levels(width, height)
    } else {
        1
    };

    // Create texture descriptor for cubemap
    // depth_or_array_layers is 6 for cubemaps (one per face)
    let descriptor =
        TextureDescriptor::new(format, Extent3d::new(width, Some(height), Some(6)), usage)
            .with_dimension(TextureDimension::N2d)
            .with_mip_level_count(mipmap_levels);

    let texture = gpu.create_texture(&descriptor.into())?;

    // Copy each face to the appropriate layer (mip level 0)
    for (face_index, face) in faces.iter().enumerate() {
        let source = face.source_info(None, None)?;
        let dest = crate::image::CopyExternalImageDestInfo::new(&texture)
            .with_origin(Origin3d::new().with_z(face_index as u32))
            .with_mip_level(0)
            .with_premultiplied_alpha(face.premultiplied_alpha());

        gpu.copy_external_image_to_texture(&source.into(), &dest.into(), &face.extent_3d().into())?;
    }

    // Generate mipmaps for the cubemap if requested
    if generate_mipmap {
        // Cubemaps occupy the entire texture, so pass empty tiles vec (no tile-aware processing needed)
        generate_mipmaps(
            gpu,
            &texture,
            &[
                MipmapTextureKind::Albedo,
                MipmapTextureKind::Albedo,
                MipmapTextureKind::Albedo,
                MipmapTextureKind::Albedo,
                MipmapTextureKind::Albedo,
                MipmapTextureKind::Albedo,
            ],
            mipmap_levels,
        )
        .await?;
    }

    Ok((texture, mipmap_levels))
}

#[cfg(test)]
mod equirect_tests {
    use super::project_equirect_faces;

    // A 4x2 equirect split left=red / right=green; every cube face should sample
    // only those source colors (projection stays in-gamut, hits real texels).
    #[test]
    fn faces_sample_source_colors() {
        let (w, h) = (4u32, 2u32);
        let mut img = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                let c = if x < w / 2 {
                    [255, 0, 0, 255]
                } else {
                    [0, 255, 0, 255]
                };
                img[o..o + 4].copy_from_slice(&c);
            }
        }
        let faces = project_equirect_faces(&img, w, h, 8);
        assert_eq!(faces.len(), 6);
        for face in &faces {
            assert_eq!(face.len(), 8 * 8 * 4);
            for px in face.chunks(4) {
                // red or green channel present, blue always 0 — a real sample.
                assert_eq!(px[2], 0, "blue leaked → sampled outside the source");
                assert!(px[0] > 0 || px[1] > 0, "black → missed the source");
            }
        }
    }
}
