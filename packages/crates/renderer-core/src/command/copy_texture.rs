//! Texel copy descriptors for buffer and texture copies.

use crate::texture::TextureAspect;

/// Source buffer info for texel copy operations.
#[derive(Debug, Clone)]
pub struct TexelCopyBufferInfo<'a> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUCommandEncoder/copyBufferToTexture#source
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuTexelCopyBufferInfo.html
    pub buffer: &'a web_sys::GpuBuffer,
    pub bytes_per_row: Option<u32>,
    pub offset: Option<u64>,
    pub rows_per_image: Option<u32>,
}

impl<'a> TexelCopyBufferInfo<'a> {
    /// Creates a buffer copy source.
    pub fn new(buffer: &'a web_sys::GpuBuffer) -> Self {
        Self {
            buffer,
            bytes_per_row: None,
            offset: None,
            rows_per_image: None,
        }
    }

    /// Sets bytes per row.
    pub fn with_bytes_per_row(mut self, bytes_per_row: u32) -> Self {
        self.bytes_per_row = Some(bytes_per_row);
        self
    }

    /// Sets the byte offset.
    pub fn with_offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Sets rows per image.
    pub fn with_rows_per_image(mut self, rows_per_image: u32) -> Self {
        self.rows_per_image = Some(rows_per_image);
        self
    }
}

/// Destination texture info for texel copy operations.
#[derive(Debug, Clone)]
pub struct TexelCopyTextureInfo<'a> {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUCommandEncoder/copyBufferToTexture#destination
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuTexelCopyTextureInfo.html
    pub texture: &'a web_sys::GpuTexture,
    pub aspect: Option<TextureAspect>,
    pub mip_level: Option<u32>,
    pub origin: Option<Origin3d>,
}

impl<'a> TexelCopyTextureInfo<'a> {
    /// Creates a texture copy destination.
    pub fn new(texture: &'a web_sys::GpuTexture) -> Self {
        Self {
            texture,
            aspect: None,
            mip_level: None,
            origin: None,
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
}

/// Layout info for texture data stored in a buffer.
#[derive(Debug, Clone, Default)]
pub struct TexelCopyBufferLayout {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUQueue/writeTexture#datalayout
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuTexelCopyBufferLayout.html
    pub bytes_per_row: Option<u32>,
    pub rows_per_image: Option<u32>,
    pub offset: Option<u64>,
}

impl TexelCopyBufferLayout {
    /// Creates a default buffer layout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets bytes per row.
    pub fn with_bytes_per_row(mut self, bytes_per_row: u32) -> Self {
        self.bytes_per_row = Some(bytes_per_row);
        self
    }

    /// Sets rows per image.
    pub fn with_rows_per_image(mut self, rows_per_image: u32) -> Self {
        self.rows_per_image = Some(rows_per_image);
        self
    }

    /// Sets the byte offset.
    pub fn with_offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }
}

/// 3D origin for texture copies.
#[derive(Debug, Clone, Default)]
pub struct Origin3d {
    // https://developer.mozilla.org/en-US/docs/Web/API/GPUCommandEncoder/copyBufferToTexture#origin
    // https://docs.rs/web-sys/latest/web_sys/struct.GpuOrigin3dDict.html
    pub x: Option<u32>,
    pub y: Option<u32>,
    pub z: Option<u32>,
}

impl Origin3d {
    /// Creates an origin at (0, 0, 0).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the x coordinate.
    pub fn with_x(mut self, x: u32) -> Self {
        self.x = Some(x);
        self
    }

    /// Sets the y coordinate.
    pub fn with_y(mut self, y: u32) -> Self {
        self.y = Some(y);
        self
    }

    /// Sets the z coordinate.
    pub fn with_z(mut self, z: u32) -> Self {
        self.z = Some(z);
        self
    }
}

// js conversion

impl From<TexelCopyBufferInfo<'_>> for web_sys::GpuTexelCopyBufferInfo {
    fn from(info: TexelCopyBufferInfo) -> Self {
        let info_js = web_sys::GpuTexelCopyBufferInfo::new(info.buffer);

        if let Some(bytes_per_row) = info.bytes_per_row {
            info_js.set_bytes_per_row(bytes_per_row);
        }
        if let Some(offset) = info.offset {
            info_js.set_offset_f64(offset as f64);
        }
        if let Some(rows_per_image) = info.rows_per_image {
            info_js.set_rows_per_image(rows_per_image);
        }

        info_js
    }
}

impl From<TexelCopyTextureInfo<'_>> for web_sys::GpuTexelCopyTextureInfo {
    fn from(info: TexelCopyTextureInfo) -> Self {
        let info_js = web_sys::GpuTexelCopyTextureInfo::new(info.texture);

        if let Some(aspect) = info.aspect {
            info_js.set_aspect(aspect);
        }
        if let Some(mip_level) = info.mip_level {
            info_js.set_mip_level(mip_level);
        }
        if let Some(origin) = info.origin {
            info_js.set_origin_gpu_origin_3d_dict(&web_sys::GpuOrigin3dDict::from(origin));
        }

        info_js
    }
}

impl From<TexelCopyBufferLayout> for web_sys::GpuTexelCopyBufferLayout {
    fn from(layout: TexelCopyBufferLayout) -> Self {
        let layout_js = web_sys::GpuTexelCopyBufferLayout::new();

        if let Some(bytes_per_row) = layout.bytes_per_row {
            layout_js.set_bytes_per_row(bytes_per_row);
        }
        if let Some(rows_per_image) = layout.rows_per_image {
            layout_js.set_rows_per_image(rows_per_image);
        }
        if let Some(offset) = layout.offset {
            layout_js.set_offset_f64(offset as f64);
        }

        layout_js
    }
}

impl From<Origin3d> for web_sys::GpuOrigin3dDict {
    fn from(origin: Origin3d) -> Self {
        let origin_js = web_sys::GpuOrigin3dDict::new();
        if let Some(x) = origin.x {
            origin_js.set_x(x);
        }
        if let Some(y) = origin.y {
            origin_js.set_y(y);
        }
        if let Some(z) = origin.z {
            origin_js.set_z(z);
        }
        origin_js
    }
}
