//! Error types for renderer-core.

use thiserror::Error;
use wasm_bindgen::prelude::*;

use crate::shaders::ShaderCompilationMessage;
use crate::texture::TextureFormat;

/// Result type for renderer-core.
pub type Result<T> = std::result::Result<T, AwsmCoreError>;

/// Core error variants for WebGPU operations.
#[derive(Error, Debug)]
pub enum AwsmCoreError {
    #[error("[gpu] Failed to create Adapter: {0}")]
    GpuAdapter(String),

    #[error("[gpu] Failed to create Device: {0}")]
    GpuDevice(String),

    #[error("[gpu] Failed to create Canvas Context: {0}")]
    CanvasContext(String),

    #[error("[gpu] Failed to configure Context: {0}")]
    ContextConfiguration(String),

    #[error("[gpu] Failed to create Pipeline from valid descriptor: {0}")]
    PipelineCreation(String),

    #[error("[gpu] Failed to create Pipeline Descriptor: {0}")]
    PipelineDescriptor(String),

    #[error("[gpu] Shader not found")]
    ShaderNotFound,

    #[error("[gpu] Failed to create query set: {0}")]
    QuerySetCreation(String),

    #[error("[gpu] Failed to create Bind Group Layout: {0}")]
    BindGroupLayout(String),

    #[error("[gpu] Failed to create External Texture: {0}")]
    ExternalTextureCreation(String),

    #[error("[gpu] Failed to create Texture: {0}")]
    TextureCreation(String),

    #[error("[gpu] Failed to create Texture View: {0}")]
    TextureView(String),

    #[error("[gpu] Failed to create RenderPass Command: {0}")]
    CommandRenderPass(String),

    #[error("[gpu] Failed to get current context texture: {0}")]
    CurrentContextTexture(String),

    #[error("[gpu] Failed to get current context texture view: {0}")]
    CurrentContextTextureView(String),

    #[error("[gpu] failed copy buffer to buffer command: {0}")]
    CommandCopyBufferToBuffer(String),

    #[error("[gpu] failed copy buffer to texture command: {0}")]
    CommandCopyBufferToTexture(String),

    #[error("[gpu] failed copy texture to buffer command: {0}")]
    CommandCopyTextureToBuffer(String),

    #[error("[gpu] failed copy texture to texture command: {0}")]
    CommandCopyTextureToTexture(String),

    #[error("[gpu] failed create buffer: {0}")]
    BufferCreation(String),

    #[error("[gpu] failed map buffer: {0}")]
    BufferMap(String),

    #[error("[gpu] failed map buffer range: {0}")]
    BufferMapRange(String),

    #[error("[gpu] failed map buffer copy: {0}")]
    BufferCopy(String),

    #[error("[gpu] failed write buffer: {0}")]
    BufferWrite(String),

    #[error("[gpu] failed write texture: {0}")]
    TextureWrite(String),

    #[error("[gpu] Failed to copy external image to texture: {0}")]
    CopyExternalImageToTexture(String),

    #[cfg(feature = "image")]
    #[error("[gpu] Image load: {0}")]
    ImageLoad(String),

    #[cfg(feature = "image")]
    #[error("[gpu] Failed to get location origin: {0}")]
    LocationOrigin(String),

    #[cfg(feature = "image")]
    #[error("[gpu] Failed to parse url: {0}")]
    UrlParse(String),

    #[cfg(feature = "exr")]
    #[error("[gpu] Failed to create js value from exr image data: {0}")]
    ExrImageToJsValue(String),

    #[cfg(feature = "ktx")]
    #[error("[gpu] ktx error: {0}")]
    Ktx(String),

    #[error("[gpu] cubemap error: {0}")]
    Cubemap(String),

    #[cfg(feature = "mega-texture")]
    #[error("[gpu] Largest Image {largest_img_width}x{largest_img_height} (padding {padding}) was too large to fit in mega texture atlas {atlas_width}x{atlas_height}")]
    MegaTextureAtlasSize {
        largest_img_width: usize,
        largest_img_height: usize,
        atlas_width: usize,
        atlas_height: usize,
        padding: usize,
    },
    #[cfg(feature = "mega-texture")]
    #[error("[gpu] mega texture depth {provided} is too small, required at least {required}")]
    MegaTextureAtlasDepthTooSmall { provided: usize, required: usize },

    #[cfg(feature = "mega-texture")]
    #[error("[gpu] mega texture duplicate id: {id}")]
    MegaTextureDuplicateId { id: String },

    #[cfg(feature = "mega-texture")]
    #[error("[gpu] mega texture index size: {0:?}")]
    MegaTextureIndexSize(std::num::TryFromIntError),

    #[cfg(feature = "mega-texture")]
    #[error("[gpu] mega texture too many atlases: {total_atlases} (max {max_atlases})")]
    MegaTextureTooManyAtlases {
        total_atlases: u32,
        max_atlases: u32,
    },

    #[cfg(feature = "texture-pool")]
    #[error("[gpu] texture pool too many arrays: {total_arrays} (max {max_arrays})")]
    TexturePoolTooManyArrays { total_arrays: u32, max_arrays: u32 },

    #[cfg(feature = "texture-pool")]
    #[error("[gpu] texture pool too many layers: {total_layers} (max {max_layers}) in array index {array_index}")]
    TexturePoolTooManyLayers {
        array_index: u32,
        total_layers: u32,
        max_layers: u32,
    },

    #[cfg(feature = "mega-texture")]
    #[error("[gpu] mega texture too many samplers: {total_samplers} (max {max_samplers})")]
    MegaTextureTooManySamplers {
        total_samplers: u32,
        max_samplers: u32,
    },

    #[cfg(feature = "texture-pool")]
    #[error("[gpu] texture pool too many samplers: {total_samplers} (max {max_samplers})")]
    TexturePoolTooManySamplers {
        total_samplers: u32,
        max_samplers: u32,
    },

    #[cfg(feature = "texture-export")]
    #[error("[gpu] texture export unsupported format: {0:?}")]
    TextureExportUnsupportedFormat(TextureFormat),

    #[cfg(feature = "texture-export")]
    #[error("[gpu] texture export failed to write: {0:?}")]
    TextureExportFailedWrite(image::ImageError),

    #[cfg(feature = "texture-export")]
    #[error("[gpu] texture export unsupported format for png encoding: {0:?}")]
    TextureExportUnsupportedPngEncoding(TextureFormat),

    #[error("[gpu] Failed to get Shader compilation info: {0}")]
    ShaderCompilationInfo(String),

    #[error("[gpu] Failed to validate Shader: {0:#?}")]
    ShaderValidation(Vec<ShaderCompilationMessage>),

    #[error("[gpu] Failed to set bind group: {0}")]
    SetBindGroup(String),

    #[error("[gpu] Failed to fetch: {0:?}")]
    Fetch(String),

    #[error("[gpu] Failed to create image bitmap: {0:?}")]
    CreateImageBitmap(String),

    #[error("[gpu] Failed to create texture: {0:?}")]
    CreateTexture(String),

    #[error("[gpu] Failed to create texture view: {0:?}")]
    CreateTextureView(String),

    #[error("[gpu] texture export unsupported format: {0:?}")]
    TextureClearUnsupportedFormat(TextureFormat),

    #[error("[gpu] mipmap generation unsupported format: {0:?}")]
    MipmapUnsupportedFormat(TextureFormat),
}

impl AwsmCoreError {
    /// Creates an adapter error from a JS value.
    pub fn gpu_adapter(err: JsValue) -> Self {
        Self::GpuAdapter(format_err(err))
    }

    /// Creates a device error from a JS value.
    pub fn gpu_device(err: JsValue) -> Self {
        Self::GpuDevice(format_err(err))
    }

    /// Creates a canvas context error from a JS value.
    pub fn canvas_context(err: JsValue) -> Self {
        Self::CanvasContext(format_err(err))
    }

    /// Creates a context configuration error from a JS value.
    pub fn context_configuration(err: JsValue) -> Self {
        Self::ContextConfiguration(format_err(err))
    }

    /// Creates a pipeline creation error from a JS value.
    pub fn pipeline_creation(err: JsValue) -> Self {
        match err.dyn_into::<web_sys::GpuPipelineError>() {
            Ok(err) => {
                let reason = match err.reason() {
                    web_sys::GpuPipelineErrorReason::Validation => "Validation",
                    web_sys::GpuPipelineErrorReason::Internal => "Internal",
                    _ => "Unknown",
                };

                Self::PipelineCreation(format!(
                    "Pipeline creation [{}] error: {}",
                    reason,
                    err.message()
                ))
            }
            Err(err) => Self::PipelineCreation(format_err(err)),
        }
    }

    /// Creates a pipeline descriptor error from a JS value.
    pub fn pipeline_descriptor(err: JsValue) -> Self {
        Self::PipelineDescriptor(format_err(err))
    }

    /// Creates a query set creation error from a JS value.
    pub fn query_set_creation(err: JsValue) -> Self {
        Self::QuerySetCreation(format_err(err))
    }

    /// Creates a bind group layout error from a JS value.
    pub fn bind_group_layout(err: JsValue) -> Self {
        Self::BindGroupLayout(format_err(err))
    }

    /// Creates an external texture creation error from a JS value.
    pub fn external_texture_creation(err: JsValue) -> Self {
        Self::ExternalTextureCreation(format_err(err))
    }

    /// Creates a texture creation error from a JS value.
    pub fn texture_creation(err: JsValue) -> Self {
        Self::TextureCreation(format_err(err))
    }

    /// Creates a texture view error from a JS value.
    pub fn texture_view(err: JsValue) -> Self {
        Self::TextureView(format_err(err))
    }

    /// Creates a render pass error from a JS value.
    pub fn command_render_pass(err: JsValue) -> Self {
        Self::CommandRenderPass(format_err(err))
    }

    /// Creates a current texture error from a JS value.
    pub fn current_context_texture(err: JsValue) -> Self {
        Self::CurrentContextTexture(format_err(err))
    }

    /// Creates a current texture view error from a JS value.
    pub fn current_context_texture_view(err: JsValue) -> Self {
        Self::CurrentContextTextureView(format_err(err))
    }

    /// Creates a buffer-to-buffer copy error from a JS value.
    pub fn command_copy_buffer_to_buffer(err: JsValue) -> Self {
        Self::CommandCopyBufferToBuffer(format_err(err))
    }

    /// Creates a buffer-to-texture copy error from a JS value.
    pub fn command_copy_buffer_to_texture(err: JsValue) -> Self {
        Self::CommandCopyBufferToTexture(format_err(err))
    }

    /// Creates a texture-to-buffer copy error from a JS value.
    pub fn command_copy_texture_to_buffer(err: JsValue) -> Self {
        Self::CommandCopyTextureToBuffer(format_err(err))
    }

    /// Creates a texture-to-texture copy error from a JS value.
    pub fn command_copy_texture_to_texture(err: JsValue) -> Self {
        Self::CommandCopyTextureToTexture(format_err(err))
    }

    /// Creates a buffer creation error from a JS value.
    pub fn buffer_creation(err: JsValue) -> Self {
        Self::BufferCreation(format_err(err))
    }

    /// Creates a buffer map error from a JS value.
    pub fn buffer_map(err: JsValue) -> Self {
        Self::BufferMap(format_err(err))
    }

    /// Creates a buffer map range error from a JS value.
    pub fn buffer_map_range(err: JsValue) -> Self {
        Self::BufferMapRange(format_err(err))
    }

    /// Creates a buffer copy error from a JS value.
    pub fn buffer_copy(err: JsValue) -> Self {
        Self::BufferCopy(format_err(err))
    }

    /// Creates a buffer write error from a JS value.
    pub fn buffer_write(err: JsValue) -> Self {
        Self::BufferWrite(format_err(err))
    }

    /// Creates a texture write error from a JS value.
    pub fn texture_write(err: JsValue) -> Self {
        Self::TextureWrite(format_err(err))
    }

    /// Creates an external image copy error from a JS value.
    pub fn copy_external_image_to_texture(err: JsValue) -> Self {
        Self::CopyExternalImageToTexture(format_err(err))
    }

    /// Creates an image load error from a JS value.
    #[cfg(feature = "image")]
    pub fn image_load(err: JsValue) -> Self {
        Self::ImageLoad(format_err(err))
    }

    /// Creates a location origin error from a JS value.
    #[cfg(feature = "image")]
    pub fn location_origin(err: JsValue) -> Self {
        Self::LocationOrigin(format_err(err))
    }

    /// Creates a URL parse error from a JS value.
    #[cfg(feature = "image")]
    pub fn url_parse(err: JsValue) -> Self {
        Self::UrlParse(format_err(err))
    }

    /// Creates an EXR conversion error from a JS value.
    #[cfg(feature = "exr")]
    pub fn exr_image_to_js_value(err: JsValue) -> Self {
        Self::ExrImageToJsValue(format_err(err))
    }

    /// Creates a shader compilation info error from a JS value.
    pub fn shader_compilation_info(err: JsValue) -> Self {
        Self::ShaderCompilationInfo(format_err(err))
    }

    /// Creates a bind group set error from a JS value.
    pub fn set_bind_group(err: JsValue) -> Self {
        Self::SetBindGroup(format_err(err))
    }

    /// Creates a fetch error from a JS value.
    pub fn fetch(err: JsValue) -> Self {
        Self::Fetch(format_err(err))
    }

    /// Creates an image bitmap error from a JS value.
    pub fn create_image_bitmap(err: JsValue) -> Self {
        Self::CreateImageBitmap(format_err(err))
    }

    /// Creates a texture creation error from a JS value.
    pub fn create_texture(err: JsValue) -> Self {
        Self::CreateTexture(format_err(err))
    }

    /// Creates a texture view creation error from a JS value.
    pub fn create_texture_view(err: JsValue) -> Self {
        Self::CreateTextureView(format_err(err))
    }
}

fn format_err(err: JsValue) -> String {
    err.as_string().unwrap_or_else(|| format!("{err:#?}"))
}
