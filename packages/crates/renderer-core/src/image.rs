//! Image loading and texture upload helpers.

use crate::command::copy_texture::{Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo};
use crate::error::{AwsmCoreError, Result};
use crate::renderer::AwsmRendererWebGpu;
use crate::texture::block_format::{
    is_block_compressed, mip_level_byte_size, rows_per_image, tight_bytes_per_row,
};
use crate::texture::mipmap::{generate_mipmaps, MipmapTextureKind};
use crate::texture::{
    mipmap, Extent3d, TextureAspect, TextureDescriptor, TextureFormat, TextureUsage,
};
use std::borrow::Cow;
use std::sync::Arc;
use wasm_bindgen::JsCast;

pub mod bitmap;
#[cfg(feature = "exr")]
pub mod exr;

/// Image source data for textures.
#[derive(Clone)]
pub enum ImageData {
    #[cfg(feature = "exr")]
    Exr(Arc<exr::ExrImage>),
    Bitmap {
        image: web_sys::ImageBitmap,
        options: Option<ImageBitmapOptions>,
    },
    /// Pre-compressed GPU block data (BC/ETC2/ASTC — e.g. a KTX2/Basis
    /// transcode result) with a pre-supplied mip chain. Uploaded verbatim via
    /// `writeTexture`; NEVER goes through `copy_external_image_to_texture`,
    /// the `srgb_to_linear` compute pass, or compute mip-gen (all invalid on
    /// compressed formats — sRGB decode is carried by the `*UnormSrgb`
    /// format instead).
    Compressed(Arc<CompressedImage>),
}

/// CPU-side block-compressed texture: one tight-layout byte buffer per mip
/// level, level 0 first, each exactly `mip_level_byte_size(format, w>>l, h>>l)`
/// long.
pub struct CompressedImage {
    pub format: TextureFormat,
    pub width: u32,
    pub height: u32,
    pub levels: Vec<Vec<u8>>,
}

impl CompressedImage {
    /// Validates level count + per-level byte sizes against the format's
    /// block layout, so malformed transcoder/container output fails here
    /// (with a real message) instead of as an opaque GPU validation error.
    pub fn validate(&self) -> Result<()> {
        if !is_block_compressed(self.format) {
            return Err(AwsmCoreError::CompressedImage(format!(
                "{:?} is not a block-compressed format",
                self.format
            )));
        }
        if self.levels.is_empty() {
            return Err(AwsmCoreError::CompressedImage(
                "compressed image has no mip levels".to_string(),
            ));
        }
        let max_levels = mipmap::calculate_mipmap_levels(self.width, self.height);
        if self.levels.len() as u32 > max_levels {
            return Err(AwsmCoreError::CompressedImage(format!(
                "{} mip levels exceeds the {} possible for {}x{}",
                self.levels.len(),
                max_levels,
                self.width,
                self.height
            )));
        }
        for (level, data) in self.levels.iter().enumerate() {
            let (w, h) = self.level_size(level as u32);
            let expected = mip_level_byte_size(self.format, w, h);
            if data.len() != expected {
                return Err(AwsmCoreError::CompressedImage(format!(
                    "mip {level} ({w}x{h} {:?}) is {} bytes, expected {expected}",
                    self.format,
                    data.len()
                )));
            }
        }
        Ok(())
    }

    /// Texel dimensions of `level`.
    pub fn level_size(&self, level: u32) -> (u32, u32) {
        (
            std::cmp::max(1, self.width >> level),
            std::cmp::max(1, self.height >> level),
        )
    }

    /// Writes every mip level of this image into `texture` at array layer
    /// `layer` via `writeTexture` (tight rows — no 256-alignment or staging
    /// needed for texture writes).
    pub fn write_to_texture_layer(
        &self,
        gpu: &AwsmRendererWebGpu,
        texture: &web_sys::GpuTexture,
        layer: u32,
    ) -> Result<()> {
        let (block_w, block_h, _) = crate::texture::block_format::block_dims(self.format)
            .ok_or_else(|| {
                AwsmCoreError::CompressedImage(format!(
                    "{:?} is not a block-compressed format",
                    self.format
                ))
            })?;
        for (level, data) in self.levels.iter().enumerate() {
            let (w, h) = self.level_size(level as u32);
            // Copies on compressed textures are validated against the
            // PHYSICAL mip size (virtual size rounded up to whole blocks):
            // the 2×2 and 1×1 tail mips of a 4×4-block format must be
            // written as 4×4, or writeTexture rejects the copy
            // ("copySize.width (2) is not a multiple of block width (4)").
            let w_phys = w.div_ceil(block_w) * block_w;
            let h_phys = h.div_ceil(block_h) * block_h;
            let destination = TexelCopyTextureInfo::new(texture)
                .with_mip_level(level as u32)
                .with_origin(Origin3d::new().with_z(layer));
            let layout = TexelCopyBufferLayout::new()
                .with_bytes_per_row(tight_bytes_per_row(self.format, w))
                .with_rows_per_image(rows_per_image(self.format, h));
            let size = Extent3d::new(w_phys, Some(h_phys), Some(1));
            gpu.write_texture(
                &destination.into(),
                data.as_slice(),
                &layout.into(),
                &size.into(),
            )?;
        }
        Ok(())
    }
}

// If we don't set premultiply, the browser will use the default, which may be to apply it - or NOT!
// Recommendation here is to set it explicitly rather than relying on defaults.
// However, for color space conversion, we want the browser to optimally try to load the image in the best color space it can.
// Since we don't have full control over the image loading process, we might as well let the browser handle it
// and then we at least *more* safely assume it's srgb, which is the most common color space for web images.
// (our EXR loader just deals with raw data)
//
// Color space handling:
// - EXR: Uses `TextureFormat::Rgba32Float` - data is already linear from the file format
// - Bitmap images: Uses `TextureFormat::Rgba8Unorm` - we convert sRGB→linear in shaders
//
// See:
// https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#dom-imagebitmapoptions-premultiplyalpha
// https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#dom-imagebitmapoptions-colorspaceconversion

impl ImageData {
    cfg_if::cfg_if! {
        if #[cfg(feature = "exr")] {
            /// Loads an image or EXR from a URL.
            pub async fn load_url(url:&str, options: Option<ImageBitmapOptions>) -> anyhow::Result<Self> {
                if url.contains(".exr") {
                    let exr_image = exr::ExrImage::load_url(url).await?;
                    Ok(Self::Exr(Arc::new(exr_image)))
                } else {
                    let image = bitmap::load(url.to_string(), options.clone()).await?;
                    Ok(Self::Bitmap{image, options})
                }
            }
        } else if #[cfg(feature = "image")] {
            /// Loads an image from a URL.
            pub async fn load_url(url:&str, options: Option<ImageBitmapOptions>) -> Result<Self> {
                let image = bitmap::load(url.to_string(), options.clone()).await?;
                Ok(Self::Bitmap{image, options})
            }
        }
    }

    /// Returns the GPU texture format for this image.
    pub fn format(&self) -> TextureFormat {
        match self {
            #[cfg(feature = "exr")]
            // EXR files use Rgba32Float and are already in linear space
            Self::Exr(_) => TextureFormat::Rgba32float,
            // We use Rgba8Unorm (not Rgba8UnormSrgb) because:
            // 1. sRGB formats don't support STORAGE usage needed for mipmap generation
            // 2. We handle sRGB→linear conversion manually in shaders for full control
            // 3. This gives us flexibility for mixed content (some textures might not be sRGB)
            //
            // Regular images use Rgba8Unorm and get converted via srgb_to_linear() in shaders
            Self::Bitmap { .. } => TextureFormat::Rgba8unorm,
            // Compressed images carry their exact block format (the sRGB-ness
            // rides in the format itself, e.g. Bc7RgbaUnormSrgb).
            Self::Compressed(compressed) => compressed.format,
        }
    }

    /// Returns whether the image is premultiplied.
    pub fn premultiplied_alpha(&self) -> bool {
        match self {
            // EXR uploads go through the SAME `copy_external_image_to_texture`
            // path as bitmaps (see `create_texture`), so this flag is live for
            // EXR. `true` is a no-op for the dominant EXR use — opaque HDR
            // environment/IBL maps have alpha = 1 everywhere, so premultiplied
            // vs straight is identical. It is UNVERIFIED for an EXR carrying
            // meaningful alpha (would need such an asset + a visual A/B to
            // confirm the loader's `js_obj()` hands the browser straight,
            // not-premultiplied, RGBA). Revisit if a non-opaque EXR ever ships.
            #[cfg(feature = "exr")]
            Self::Exr(_) => true,

            Self::Bitmap { options, .. } => options
                .as_ref()
                .map(|opts| matches!(opts.premultiply_alpha, Some(PremultiplyAlpha::Premultiply)))
                .unwrap_or(false),

            // Block data is stored straight (non-premultiplied); the flag is
            // only consumed by `copy_external_image_to_texture`, which
            // compressed uploads never touch.
            Self::Compressed(_) => false,
        }
    }

    /// Returns the image size in pixels.
    pub fn size(&self) -> (u32, u32) {
        match self {
            #[cfg(feature = "exr")]
            Self::Exr(exr) => (exr.width as u32, exr.height as u32),
            Self::Bitmap { image, .. } => (image.width(), image.height()),
            Self::Compressed(compressed) => (compressed.width, compressed.height),
        }
    }

    /// Returns the image size as a texture extent.
    pub fn extent_3d(&self) -> Extent3d {
        match self {
            #[cfg(feature = "exr")]
            Self::Exr(exr) => Extent3d {
                width: exr.width as u32,
                height: Some(exr.height as u32),
                depth_or_array_layers: None,
            },

            Self::Bitmap { image, .. } => Extent3d {
                width: image.width(),
                height: Some(image.height()),
                depth_or_array_layers: None,
            },

            Self::Compressed(compressed) => Extent3d {
                width: compressed.width,
                height: Some(compressed.height),
                depth_or_array_layers: None,
            },
        }
    }

    /// Returns the JS object used for external image copies.
    pub fn js_obj(&self) -> Result<Cow<'_, js_sys::Object>> {
        match self {
            #[cfg(feature = "exr")]
            Self::Exr(exr) => exr.js_obj(),

            Self::Bitmap { image, .. } => {
                let js_value = image.unchecked_ref();
                Ok(Cow::Borrowed(js_value))
            }

            // No JS-side object exists — compressed data uploads via
            // `writeTexture` (`CompressedImage::write_to_texture_layer`),
            // never via external-image copy. Callers must branch on
            // `is_compressed()` first.
            Self::Compressed(_) => Err(AwsmCoreError::CompressedImage(
                "compressed images have no external-image source; upload via writeTexture"
                    .to_string(),
            )),
        }
    }

    /// Whether this is pre-compressed block data (uploads via `writeTexture`,
    /// no external-image copy / sRGB pass / compute mip-gen).
    pub fn is_compressed(&self) -> bool {
        matches!(self, Self::Compressed(_))
    }

    /// The compressed payload, when [`Self::is_compressed`].
    pub fn as_compressed(&self) -> Option<&Arc<CompressedImage>> {
        match self {
            Self::Compressed(compressed) => Some(compressed),
            _ => None,
        }
    }

    /// Builds a copy source info for `copy_external_image_to_texture`.
    pub fn source_info(
        &self,
        origin: Option<[f32; 2]>,
        flip_y: Option<bool>,
    ) -> Result<CopyExternalImageSourceInfo<'_>> {
        Ok(CopyExternalImageSourceInfo {
            flip_y,
            origin,
            source: self.js_obj()?,
        })
    }

    /// Creates a GPU texture and optionally generates mipmaps.
    pub async fn create_texture(
        &self,
        gpu: &AwsmRendererWebGpu,
        source_info: Option<CopyExternalImageSourceInfo<'_>>,
        mipmap_kind: Option<MipmapTextureKind>,
        // if None, will try to determine from source image options
        premultiply_alpha: Option<bool>,
    ) -> Result<web_sys::GpuTexture> {
        // Compressed data takes the writeTexture path: pre-supplied mips,
        // no external-image copy, no storage usage, no compute mip-gen.
        if let Self::Compressed(compressed) = self {
            compressed.validate()?;
            let usage = TextureUsage::new().with_texture_binding().with_copy_dst();
            let descriptor = TextureDescriptor::new(self.format(), self.extent_3d(), usage)
                .with_mip_level_count(compressed.levels.len() as u32);
            let texture = gpu.create_texture(&descriptor.into())?;
            compressed.write_to_texture_layer(gpu, &texture, 0)?;
            return Ok(texture);
        }

        let mut usage = TextureUsage::new()
            .with_texture_binding()
            // needed because `copy_external_image_to_texture` renders to the texture internally, part of browser WebGPU implementation
            .with_render_attachment()
            .with_copy_dst();

        if mipmap_kind.is_some() {
            usage = usage.with_storage_binding();
        }

        let source = match source_info {
            Some(info) => info,
            None => CopyExternalImageSourceInfo {
                flip_y: None,
                origin: None,
                source: self.js_obj()?,
            },
        };

        let mut descriptor = TextureDescriptor::new(self.format(), self.extent_3d(), usage);
        let mipmap_levels = if mipmap_kind.is_some() {
            let (width, height) = self.size();

            let mipmap_levels = mipmap::calculate_mipmap_levels(width, height);

            descriptor = descriptor.with_mip_level_count(mipmap_levels);

            mipmap_levels
        } else {
            0
        };

        let texture = gpu.create_texture(&descriptor.into())?;

        let mut dest = CopyExternalImageDestInfo::new(&texture)
            .with_premultiplied_alpha(premultiply_alpha.unwrap_or(self.premultiplied_alpha()));

        if mipmap_kind.is_some() {
            dest = dest.with_mip_level(0);
        }
        gpu.copy_external_image_to_texture(&source.into(), &dest.into(), &self.extent_3d().into())?;

        if let Some(mipmap_kind) = mipmap_kind {
            generate_mipmaps(gpu, &texture, &[mipmap_kind], mipmap_levels).await?;
        }

        Ok(texture)
    }
}

/// Options for creating an image bitmap.
#[derive(Clone, Debug, Default)]
pub struct ImageBitmapOptions {
    // https://docs.rs/web-sys/latest/web_sys/struct.ImageBitmapOptions.html
    pub color_space_conversion: Option<ColorSpaceConversion>,
    pub image_orientation: Option<ImageOrientation>,
    pub premultiply_alpha: Option<PremultiplyAlpha>,
    pub resize_height: Option<u32>,
    pub resize_width: Option<u32>,
    pub resize_quality: Option<ResizeQuality>,
}

impl ImageBitmapOptions {
    /// Creates default image bitmap options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets color space conversion behavior.
    pub fn with_color_space_conversion(
        mut self,
        color_space_conversion: ColorSpaceConversion,
    ) -> Self {
        self.color_space_conversion = Some(color_space_conversion);
        self
    }

    /// Sets the image orientation.
    pub fn with_image_orientation(mut self, image_orientation: ImageOrientation) -> Self {
        self.image_orientation = Some(image_orientation);
        self
    }

    /// Sets premultiply alpha behavior.
    pub fn with_premultiply_alpha(mut self, premultiply_alpha: PremultiplyAlpha) -> Self {
        self.premultiply_alpha = Some(premultiply_alpha);
        self
    }

    /// Sets resize height.
    pub fn with_resize_height(mut self, resize_height: u32) -> Self {
        self.resize_height = Some(resize_height);
        self
    }

    /// Sets resize width.
    pub fn with_resize_width(mut self, resize_width: u32) -> Self {
        self.resize_width = Some(resize_width);
        self
    }

    /// Sets resize quality.
    pub fn with_resize_quality(mut self, resize_quality: ResizeQuality) -> Self {
        self.resize_quality = Some(resize_quality);
        self
    }
}

/// Web image color space conversion mode.
// https://docs.rs/web-sys/latest/web_sys/enum.ColorSpaceConversion.html
/// Image color space conversion setting.
pub type ColorSpaceConversion = web_sys::ColorSpaceConversion;
/// Web image orientation mode.
// https://docs.rs/web-sys/latest/web_sys/enum.ImageOrientation.html
/// Image orientation metadata handling.
pub type ImageOrientation = web_sys::ImageOrientation;
/// Web image premultiply alpha mode.
// https://docs.rs/web-sys/latest/web_sys/enum.PremultiplyAlpha.html
/// Premultiply alpha option for image decoding.
pub type PremultiplyAlpha = web_sys::PremultiplyAlpha;
/// Web image resize quality.
// https://docs.rs/web-sys/latest/web_sys/enum.ResizeQuality.html
/// Image resize quality hint.
pub type ResizeQuality = web_sys::ResizeQuality;

// Can create this from ImageData.source_info()
/// Source info for `copy_external_image_to_texture`.
pub struct CopyExternalImageSourceInfo<'a> {
    pub flip_y: Option<bool>,
    pub origin: Option<[f32; 2]>,
    pub source: Cow<'a, js_sys::Object>,
}

impl<'a> CopyExternalImageSourceInfo<'a> {
    /// Creates a source info wrapper.
    pub fn new(source: Cow<'a, js_sys::Object>) -> Self {
        Self {
            flip_y: None,
            origin: None,
            source,
        }
    }
}

/// Destination info for `copy_external_image_to_texture`.
pub struct CopyExternalImageDestInfo<'a> {
    pub texture: &'a web_sys::GpuTexture,
    pub aspect: Option<TextureAspect>,
    pub mip_level: Option<u32>,
    pub origin: Option<Origin3d>,
    pub premultiplied_alpha: Option<bool>,
}

impl<'a> CopyExternalImageDestInfo<'a> {
    /// Creates a destination info wrapper.
    pub fn new(texture: &'a web_sys::GpuTexture) -> Self {
        Self {
            aspect: None,
            mip_level: None,
            origin: None,
            premultiplied_alpha: None,
            texture,
        }
    }

    /// Sets the texture aspect.
    pub fn with_aspect(mut self, aspect: TextureAspect) -> Self {
        self.aspect = Some(aspect);
        self
    }
    /// Sets the mip level.
    pub fn with_mip_level(mut self, mip_level: u32) -> Self {
        self.mip_level = Some(mip_level);
        self
    }
    /// Sets the copy origin.
    pub fn with_origin(mut self, origin: Origin3d) -> Self {
        self.origin = Some(origin);
        self
    }
    /// Sets premultiplied alpha behavior.
    pub fn with_premultiplied_alpha(mut self, premultiplied_alpha: bool) -> Self {
        self.premultiplied_alpha = Some(premultiplied_alpha);
        self
    }
}

impl From<CopyExternalImageSourceInfo<'_>> for web_sys::GpuCopyExternalImageSourceInfo {
    fn from(info: CopyExternalImageSourceInfo) -> Self {
        // https://developer.mozilla.org/en-US/docs/Web/API/GPUQueue/copyExternalImageToTexture#source
        // https://docs.rs/web-sys/latest/web_sys/struct.GpuCopyExternalImageSourceInfo.html
        // The source is any `GPUImageCopyExternalImage` compatible JS object;
        // cast to ImageBitmap for the constructor - the underlying type is verified at runtime by WebGPU.
        let info_js =
            web_sys::GpuCopyExternalImageSourceInfo::new(info.source.as_ref().unchecked_ref());

        if let Some(flip_y) = info.flip_y {
            info_js.set_flip_y(flip_y);
        }

        if let Some(origin) = info.origin {
            info_js.set_origin(&[
                js_sys::Number::from(origin[0] as f64),
                js_sys::Number::from(origin[1] as f64),
            ]);
        }

        info_js
    }
}

impl From<CopyExternalImageDestInfo<'_>> for web_sys::GpuCopyExternalImageDestInfo {
    fn from(info: CopyExternalImageDestInfo) -> Self {
        // https://developer.mozilla.org/en-US/docs/Web/API/GPUQueue/copyExternalImageToTexture#destination
        // https://docs.rs/web-sys/latest/web_sys/struct.GpuCopyExternalImageDestInfo.html
        let info_js = web_sys::GpuCopyExternalImageDestInfo::new(info.texture);

        if let Some(aspect) = info.aspect {
            info_js.set_aspect(aspect);
        }
        if let Some(mip_level) = info.mip_level {
            info_js.set_mip_level(mip_level);
        }
        if let Some(origin) = info.origin {
            info_js.set_origin_gpu_origin_3d_dict(&web_sys::GpuOrigin3dDict::from(origin));
        }
        if let Some(premultiplied_alpha) = info.premultiplied_alpha {
            info_js.set_premultiplied_alpha(premultiplied_alpha);
        }

        info_js
    }
}

impl From<ImageBitmapOptions> for web_sys::ImageBitmapOptions {
    fn from(options: ImageBitmapOptions) -> web_sys::ImageBitmapOptions {
        let js_options = web_sys::ImageBitmapOptions::new();

        if let Some(color_space_conversion) = options.color_space_conversion {
            js_options.set_color_space_conversion(color_space_conversion);
        }

        if let Some(image_orientation) = options.image_orientation {
            js_options.set_image_orientation(image_orientation);
        }

        if let Some(premultiply_alpha) = options.premultiply_alpha {
            js_options.set_premultiply_alpha(premultiply_alpha);
        }

        if let Some(resize_height) = options.resize_height {
            js_options.set_resize_height(resize_height);
        }

        if let Some(resize_width) = options.resize_width {
            js_options.set_resize_width(resize_width);
        }

        if let Some(resize_quality) = options.resize_quality {
            js_options.set_resize_quality(resize_quality);
        }

        js_options
    }
}
