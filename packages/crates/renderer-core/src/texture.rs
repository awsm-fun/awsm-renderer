//! Texture descriptors and helpers.

#[cfg(feature = "texture-export")]
pub mod exporter;
// #[cfg(feature = "mega-texture")]
// pub mod mega_texture;
#[cfg(feature = "texture-pool")]
pub mod texture_pool;

pub mod blit;
pub mod clear;
pub mod convert_srgb;
pub mod mipmap;

use wasm_bindgen::convert::IntoWasmAbi;
use wasm_bindgen::JsCast;

/// WebGPU texture format.
// https://docs.rs/web-sys/latest/web_sys/enum.GpuTextureFormat.html
/// WebGPU texture format.
pub type TextureFormat = web_sys::GpuTextureFormat;
/// WebGPU texture aspect.
pub type TextureAspect = web_sys::GpuTextureAspect;
// https://docs.rs/web-sys/latest/web_sys/enum.GpuTextureViewDimension.html
/// WebGPU texture view dimension.
pub type TextureViewDimension = web_sys::GpuTextureViewDimension;
// https://docs.rs/web-sys/latest/web_sys/enum.GpuTextureSampleType.html
/// WebGPU texture sample type.
pub type TextureSampleType = web_sys::GpuTextureSampleType;
// https://docs.rs/web-sys/latest/web_sys/enum.GpuTextureDimension.html
/// WebGPU texture dimension.
pub type TextureDimension = web_sys::GpuTextureDimension;

/// Builder for a GPU texture descriptor.
#[derive(Debug, Clone)]
pub struct TextureDescriptor<'a> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createTexture#descriptor
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuTextureDescriptor.html
    pub format: TextureFormat,
    pub size: Extent3d,
    pub usage: TextureUsage,
    pub dimension: Option<TextureDimension>,
    pub label: Option<&'a str>,
    pub mip_level_count: Option<u32>,
    pub sample_count: Option<u32>,
    pub view_formats: Vec<TextureFormat>,
}

impl<'a> TextureDescriptor<'a> {
    /// Creates a texture descriptor with required fields.
    pub fn new(format: TextureFormat, size: Extent3d, usage: TextureUsage) -> Self {
        Self {
            dimension: None,
            format,
            label: None,
            mip_level_count: None,
            sample_count: None,
            size,
            usage,
            view_formats: Vec::new(),
        }
    }

    /// Sets the descriptor label.
    pub fn with_label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
    /// Sets the mip level count.
    pub fn with_mip_level_count(mut self, mip_level_count: u32) -> Self {
        self.mip_level_count = Some(mip_level_count);
        self
    }
    /// Sets the sample count.
    pub fn with_sample_count(mut self, sample_count: u32) -> Self {
        self.sample_count = Some(sample_count);
        self
    }
    /// Sets the texture dimension.
    pub fn with_dimension(mut self, dimension: TextureDimension) -> Self {
        self.dimension = Some(dimension);
        self
    }
    /// Adds a view format.
    pub fn with_push_view_format(mut self, view_format: TextureFormat) -> Self {
        self.view_formats.push(view_format);
        self
    }
}

/// Texture usage flags.
#[derive(Debug, Clone, Default)]
pub struct TextureUsage {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUTexture/usage
    // https://docs.rs/web-sys/latest/web_sys/gpu_texture_usage/index.html
    pub copy_dst: bool,
    pub copy_src: bool,
    pub render_attachment: bool,
    pub storage_binding: bool,
    pub texture_binding: bool,
}

impl TextureUsage {
    /// Creates an empty usage set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the WebGPU usage bitfield.
    pub fn as_u32(&self) -> u32 {
        let mut usage = 0;
        if self.copy_dst {
            usage |= web_sys::gpu_texture_usage::COPY_DST;
        }
        if self.copy_src {
            usage |= web_sys::gpu_texture_usage::COPY_SRC;
        }
        if self.render_attachment {
            usage |= web_sys::gpu_texture_usage::RENDER_ATTACHMENT;
        }
        if self.storage_binding {
            usage |= web_sys::gpu_texture_usage::STORAGE_BINDING;
        }
        if self.texture_binding {
            usage |= web_sys::gpu_texture_usage::TEXTURE_BINDING;
        }

        usage
    }

    /// Enables COPY_DST usage.
    pub fn with_copy_dst(mut self) -> Self {
        self.copy_dst = true;
        self
    }
    /// Enables COPY_SRC usage.
    pub fn with_copy_src(mut self) -> Self {
        self.copy_src = true;
        self
    }
    /// Enables RENDER_ATTACHMENT usage.
    pub fn with_render_attachment(mut self) -> Self {
        self.render_attachment = true;
        self
    }
    /// Enables STORAGE_BINDING usage.
    pub fn with_storage_binding(mut self) -> Self {
        self.storage_binding = true;
        self
    }
    /// Enables TEXTURE_BINDING usage.
    pub fn with_texture_binding(mut self) -> Self {
        self.texture_binding = true;
        self
    }
}

/// Size for a 1D/2D/3D texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent3d {
    pub width: u32,
    pub height: Option<u32>,
    pub depth_or_array_layers: Option<u32>,
}

impl Extent3d {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createTexture#size

    /// Creates a texture extent.
    pub fn new(width: u32, height: Option<u32>, depth_or_array_layers: Option<u32>) -> Self {
        Self {
            width,
            height,
            depth_or_array_layers,
        }
    }

    /// Sets the height.
    pub fn with_height(mut self, height: u32) -> Self {
        self.height = Some(height);
        self
    }

    /// Sets depth or array layers.
    pub fn with_depth_or_array_layers(mut self, depth_or_array_layers: u32) -> Self {
        self.depth_or_array_layers = Some(depth_or_array_layers);
        self
    }
}

/// Builder for a texture view descriptor.
#[derive(Debug, Clone, Default)]
pub struct TextureViewDescriptor<'a> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUTexture/createView#descriptor
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuTextureViewDescriptor.html
    pub array_layer_count: Option<u32>,
    pub aspect: Option<TextureAspect>,
    pub base_array_layer: Option<u32>,
    pub base_mip_level: Option<u32>,
    pub dimension: Option<TextureViewDimension>,
    pub format: Option<TextureFormat>,
    pub label: Option<&'a str>,
    pub mip_level_count: Option<u32>,
    pub usage: Option<TextureUsage>,
}

impl<'a> TextureViewDescriptor<'a> {
    /// Creates a texture view descriptor with an optional label.
    pub fn new(label: Option<&'a str>) -> Self {
        Self {
            label,
            ..Default::default()
        }
    }

    /// Sets the array layer count.
    pub fn with_array_layer_count(mut self, array_layer_count: u32) -> Self {
        self.array_layer_count = Some(array_layer_count);
        self
    }
    /// Sets the aspect.
    pub fn with_aspect(mut self, aspect: TextureAspect) -> Self {
        self.aspect = Some(aspect);
        self
    }
    /// Sets the base array layer.
    pub fn with_base_array_layer(mut self, base_array_layer: u32) -> Self {
        self.base_array_layer = Some(base_array_layer);
        self
    }
    /// Sets the base mip level.
    pub fn with_base_mip_level(mut self, base_mip_level: u32) -> Self {
        self.base_mip_level = Some(base_mip_level);
        self
    }
    /// Sets the view dimension.
    pub fn with_dimension(mut self, dimension: TextureViewDimension) -> Self {
        self.dimension = Some(dimension);
        self
    }
    /// Sets the view format.
    pub fn with_format(mut self, format: TextureFormat) -> Self {
        self.format = Some(format);
        self
    }
    /// Sets the mip level count.
    pub fn with_mip_level_count(mut self, mip_level_count: u32) -> Self {
        self.mip_level_count = Some(mip_level_count);
        self
    }
    /// Sets the usage flags.
    pub fn with_usage(mut self, usage: TextureUsage) -> Self {
        self.usage = Some(usage);
        self
    }
    /// Sets the view label.
    pub fn with_label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
}

/// Descriptor for importing an external texture.
pub struct ExternalTextureDescriptor<'a> {
    pub source: ExternalTextureDescriptorSource,
    pub label: Option<&'a str>,
}

impl<'a> ExternalTextureDescriptor<'a> {
    /// Creates an external texture descriptor.
    pub fn new(source: ExternalTextureDescriptorSource, label: Option<&'a str>) -> Self {
        Self { source, label }
    }
}

/// Source for an external texture.
pub enum ExternalTextureDescriptorSource {
    /// External texture from a video element.
    VideoElement(web_sys::HtmlVideoElement),
    /// External texture from a video frame.
    VideoFrame(web_sys::VideoFrame),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct TextureFormatKey(TextureFormat);

impl From<TextureFormat> for TextureFormatKey {
    fn from(format: TextureFormat) -> Self {
        Self(format)
    }
}

impl From<TextureFormatKey> for TextureFormat {
    fn from(key: TextureFormatKey) -> Self {
        key.0
    }
}

impl std::hash::Hash for TextureFormatKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.into_abi().hash(state);
    }
}

impl PartialOrd for TextureFormatKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TextureFormatKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.into_abi().cmp(&other.0.into_abi())
    }
}

/// Maps a texture format to a WGSL storage format string.
pub fn texture_format_to_wgsl_storage(format: TextureFormat) -> crate::error::Result<&'static str> {
    match format {
        TextureFormat::Rgba8unorm => Ok("rgba8unorm"),
        TextureFormat::Rgba16float => Ok("rgba16float"),
        TextureFormat::Rgba32float => Ok("rgba32float"),
        _ => Err(crate::error::AwsmCoreError::MipmapUnsupportedFormat(format)),
    }
}

// js conversions

impl From<TextureDescriptor<'_>> for web_sys::GpuTextureDescriptor {
    fn from(descriptor: TextureDescriptor) -> Self {
        let descriptor_js = web_sys::GpuTextureDescriptor::new_with_gpu_extent_3d_dict(
            descriptor.format,
            &web_sys::GpuExtent3dDict::from(descriptor.size),
            descriptor.usage.as_u32(),
        );

        if let Some(dimension) = descriptor.dimension {
            descriptor_js.set_dimension(dimension);
        }

        if let Some(label) = descriptor.label {
            descriptor_js.set_label(label);
        }

        if let Some(mip_level_count) = descriptor.mip_level_count {
            descriptor_js.set_mip_level_count(mip_level_count);
        }
        if let Some(sample_count) = descriptor.sample_count {
            descriptor_js.set_sample_count(sample_count);
        }
        if !descriptor.view_formats.is_empty() {
            let view_formats: Vec<js_sys::JsString> = descriptor
                .view_formats
                .into_iter()
                .map(|format| {
                    wasm_bindgen::JsValue::from(format).unchecked_into::<js_sys::JsString>()
                })
                .collect();
            descriptor_js.set_view_formats(&view_formats);
        }

        descriptor_js
    }
}

impl From<TextureViewDescriptor<'_>> for web_sys::GpuTextureViewDescriptor {
    fn from(descriptor: TextureViewDescriptor) -> Self {
        let descriptor_js = web_sys::GpuTextureViewDescriptor::new();

        if let Some(array_layer_count) = descriptor.array_layer_count {
            descriptor_js.set_array_layer_count(array_layer_count);
        }
        if let Some(aspect) = descriptor.aspect {
            descriptor_js.set_aspect(aspect);
        }
        if let Some(base_array_layer) = descriptor.base_array_layer {
            descriptor_js.set_base_array_layer(base_array_layer);
        }
        if let Some(base_mip_level) = descriptor.base_mip_level {
            descriptor_js.set_base_mip_level(base_mip_level);
        }
        if let Some(dimension) = descriptor.dimension {
            descriptor_js.set_dimension(dimension);
        }
        if let Some(format) = descriptor.format {
            descriptor_js.set_format(format);
        }
        if let Some(label) = descriptor.label {
            descriptor_js.set_label(label);
        }
        if let Some(mip_level_count) = descriptor.mip_level_count {
            descriptor_js.set_mip_level_count(mip_level_count);
        }
        if let Some(usage) = descriptor.usage {
            descriptor_js.set_usage(usage.as_u32());
        }

        descriptor_js
    }
}

impl From<ExternalTextureDescriptor<'_>> for web_sys::GpuExternalTextureDescriptor {
    fn from(descriptor: ExternalTextureDescriptor) -> Self {
        let descriptor_js = match &descriptor.source {
            ExternalTextureDescriptorSource::VideoElement(video) => {
                web_sys::GpuExternalTextureDescriptor::new(video)
            }
            ExternalTextureDescriptorSource::VideoFrame(frame) => {
                web_sys::GpuExternalTextureDescriptor::new_with_video_frame(frame)
            }
        };

        if let Some(label) = descriptor.label {
            descriptor_js.set_label(label);
        }

        descriptor_js
    }
}

impl From<Extent3d> for web_sys::GpuExtent3dDict {
    fn from(size: Extent3d) -> Self {
        // https://developer.mozilla.org/en-US/docs/Web/API/GPUDevice/createTexture#size
        // https://docs.rs/web-sys/latest/web_sys/struct.GpuExtent3dDict.html
        let size_js = web_sys::GpuExtent3dDict::new(size.width);

        if let Some(height) = size.height {
            size_js.set_height(height);
        }
        if let Some(depth_or_array_layers) = size.depth_or_array_layers {
            size_js.set_depth_or_array_layers(depth_or_array_layers);
        }

        size_js
    }
}
