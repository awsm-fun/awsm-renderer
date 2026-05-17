//! Texture pooling and upload helpers.

#[cfg(feature = "serde")]
pub mod report;

use std::sync::LazyLock;
use std::{collections::HashMap, hash::Hash};

use indexmap::IndexMap;

use crate::texture::TextureUsage;
use crate::{
    command::copy_texture::{Origin3d, TexelCopyTextureInfo},
    error::{AwsmCoreError, Result},
    image::{CopyExternalImageDestInfo, ImageData},
    renderer::AwsmRendererWebGpu,
    texture::{
        convert_srgb::convert_srgb_to_linear,
        mipmap::{calculate_mipmap_levels, generate_mipmaps, MipmapTextureKind},
        Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureFormatKey,
        TextureViewDescriptor, TextureViewDimension,
    },
};

/// Pool of texture arrays grouped by size and format.
///
/// `free_layers` is the slot recycler. When an ID is removed, its layer
/// index lands here per-array; the next `add_image` of a matching
/// `(width, height, format)` reuses the slot instead of pushing a fresh
/// layer onto the GPU array. This keeps the high-water-mark layer count
/// bounded across long editing sessions where the same logical texture
/// keeps being re-uploaded under different IDs (procedural-texture
/// param edits in the editor are the canonical case).
pub struct TexturePool<ID> {
    arrays: IndexMap<TexturePoolArrayKey, TexturePoolArray<ID>>,
    id_to_array_key: HashMap<ID, TexturePoolArrayKey>,
    free_layers: HashMap<TexturePoolArrayKey, Vec<usize>>,
}

/// Single texture array in the pool.
pub struct TexturePoolArray<ID> {
    pub format: TextureFormat,
    pub width: u32,
    pub height: u32,
    pub mipmap: bool,
    pub images: Vec<(ID, ImageData, TextureColorInfo)>,
    pub gpu_dirty: bool,
    pub gpu_texture: Option<web_sys::GpuTexture>,
    pub gpu_texture_view: Option<web_sys::GpuTextureView>,
}

/// Lookup information for a texture entry in the pool.
pub struct TexturePoolEntryInfo<ID> {
    pub id: ID,
    pub array_index: usize,
    pub layer_index: usize,
    pub color: TextureColorInfo,
}

/// Color and mipmap settings for pooled textures.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Ord, PartialOrd)]
pub struct TextureColorInfo {
    pub mipmap_kind: MipmapTextureKind,
    // Whether to convert from sRGB to linear color space
    pub srgb_to_linear: bool,
    // If None, use image's premultiplied alpha setting
    pub premultiplied_alpha: Option<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Ord, PartialOrd)]
struct TexturePoolArrayKey {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormatKey,
}

impl<ID: Eq + Hash + Clone> Default for TexturePool<ID> {
    fn default() -> Self {
        Self::new()
    }
}

impl<ID: Eq + Hash + Clone> TexturePool<ID> {
    /// Creates an empty texture pool.
    pub fn new() -> Self {
        Self {
            arrays: IndexMap::new(),
            id_to_array_key: HashMap::new(),
            free_layers: HashMap::new(),
        }
    }

    /// Adds an image to the pool.
    pub fn add_image(
        &mut self,
        id: ID,
        image: ImageData,
        format: TextureFormat,
        color: TextureColorInfo,
    ) {
        let (width, height) = image.size();

        let array_key = TexturePoolArrayKey {
            width,
            height,
            format: format.into(),
        };

        let array = self
            .arrays
            .entry(array_key)
            .or_insert_with(|| TexturePoolArray::new(format, width, height));

        // Prefer a recycled slot from an earlier `remove` over pushing a
        // fresh layer onto the GPU array — keeps `images.len()` (and
        // therefore the GPU array's layer count) at the historical
        // high-water mark across re-upload churn.
        if let Some(free_idx) = self.free_layers.get_mut(&array_key).and_then(|v| v.pop()) {
            array.images[free_idx] = (id.clone(), image, color);
            array.gpu_dirty = true;
        } else {
            array.insert(id.clone(), image, color);
        }

        self.id_to_array_key.insert(id, array_key);
    }

    /// Removes an ID from the pool. The freed layer slot is recycled
    /// by the next matching `add_image` so the array's GPU layer count
    /// stays bounded; the on-GPU pixels remain (as no-longer-referenced
    /// "ghost" data) until that recycle overwrites them. Returns the
    /// entry info the ID resolved to before removal, or `None` if the
    /// ID wasn't present.
    pub fn remove(&mut self, id: ID) -> Option<TexturePoolEntryInfo<ID>> {
        let array_key = self.id_to_array_key.remove(&id)?;
        let array_index = self.arrays.get_index_of(&array_key)?;
        let array = self.arrays.get(&array_key)?;
        let layer_index = array
            .images
            .iter()
            .position(|(layer_id, _, _)| *layer_id == id)?;
        let color = array.images[layer_index].2;
        self.free_layers
            .entry(array_key)
            .or_default()
            .push(layer_index);
        Some(TexturePoolEntryInfo {
            id,
            array_index,
            layer_index,
            color,
        })
    }

    /// Returns a texture array by index.
    pub fn array_by_index(&self, index: usize) -> Option<&TexturePoolArray<ID>> {
        self.arrays.values().nth(index)
    }

    /// Returns the number of texture arrays in the pool.
    pub fn arrays_len(&self) -> usize {
        self.arrays.len()
    }

    /// Looks up an entry by its ID.
    pub fn entry(&self, id: ID) -> Option<TexturePoolEntryInfo<ID>> {
        let id_array_key = self.id_to_array_key.get(&id)?;

        for (array_index, (array_key, array)) in self.arrays.iter().enumerate() {
            if id_array_key == array_key {
                for (layer_index, (layer_id, _, color)) in array.images.iter().enumerate() {
                    if *layer_id == id {
                        return Some(TexturePoolEntryInfo {
                            id,
                            array_index,
                            layer_index,
                            color: *color,
                        });
                    }
                }
            }
        }

        None
    }

    /// Uploads dirty arrays to the GPU. Returns true if any array was dirty.
    pub async fn write_gpu(&mut self, gpu: &AwsmRendererWebGpu) -> Result<bool> {
        let mut any_dirty = false;
        for array in self.arrays.values_mut() {
            any_dirty |= array.gpu_dirty;
            array.write_gpu(gpu).await?;
        }

        Ok(any_dirty)
    }

    /// Iterates over GPU textures for uploaded arrays.
    pub fn textures(&self) -> impl Iterator<Item = &web_sys::GpuTexture> {
        self.arrays
            .values()
            .filter_map(|array| array.gpu_texture.as_ref())
    }

    /// Iterates over GPU texture views for uploaded arrays.
    pub fn texture_views(&self) -> impl Iterator<Item = &web_sys::GpuTextureView> {
        self.arrays
            .values()
            .filter_map(|array| array.gpu_texture_view.as_ref())
    }
}

impl<ID> TexturePoolArray<ID> {
    /// Creates a new texture array for a format and size.
    pub fn new(format: TextureFormat, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
            mipmap: true,
            images: Vec::new(),
            gpu_dirty: true,
            gpu_texture: None,
            gpu_texture_view: None,
        }
    }

    // returns the index of the inserted image
    /// Inserts an image into the array.
    pub fn insert(&mut self, id: ID, image: ImageData, color: TextureColorInfo) {
        self.images.push((id, image, color));
        self.gpu_dirty = true;
    }

    /// Returns the mip level count for this array.
    pub fn mipmap_levels(&self) -> u32 {
        if self.mipmap {
            calculate_mipmap_levels(self.width, self.height)
        } else {
            1
        }
    }

    /// Uploads this array to the GPU if dirty.
    pub async fn write_gpu(&mut self, gpu: &AwsmRendererWebGpu) -> Result<()> {
        if !self.gpu_dirty {
            return Ok(());
        }

        let mipmap_levels = self.mipmap_levels();
        let layers = self.images.len() as u32;

        let texture_usage = if self.mipmap {
            TEXTURE_USAGE_MIPMAP.clone()
        } else {
            TEXTURE_USAGE_NO_MIPMAP.clone()
        };

        let dest_tex = gpu.create_texture(
            &TextureDescriptor::new(
                self.format,
                Extent3d::new(self.width, Some(self.height), Some(layers)),
                texture_usage,
            )
            .with_label("Texture Pool Array Dest")
            .with_mip_level_count(mipmap_levels)
            .with_dimension(TextureDimension::N2d)
            .into(),
        )?;

        let mut mipmap_texture_kinds: Vec<MipmapTextureKind> = Vec::new();

        // Staging textures usage flags
        let staging_usage = TextureUsage::new()
            .with_texture_binding()
            .with_storage_binding()
            .with_copy_dst()
            .with_copy_src()
            .with_render_attachment();

        // Process each layer: copy to staging, optionally convert, then copy to dest array
        for (index, (_, image, color)) in self.images.iter().enumerate() {
            // Create fresh staging textures for this layer to avoid GPU race conditions
            let staging_src = gpu.create_texture(
                &TextureDescriptor::new(
                    self.format,
                    Extent3d::new(self.width, Some(self.height), Some(1)),
                    staging_usage.clone(),
                )
                .with_label("Texture Pool Staging Src")
                .with_dimension(TextureDimension::N2d)
                .into(),
            )?;

            let staging_dst = gpu.create_texture(
                &TextureDescriptor::new(
                    self.format,
                    Extent3d::new(self.width, Some(self.height), Some(1)),
                    staging_usage.clone(),
                )
                .with_label("Texture Pool Staging Dst")
                .with_dimension(TextureDimension::N2d)
                .into(),
            )?;

            // Copy external image to staging_src (this is a queue operation)
            let source = image.source_info(None, None)?;
            let staging_dest = CopyExternalImageDestInfo::new(&staging_src)
                .with_mip_level(0)
                .with_premultiplied_alpha(
                    color
                        .premultiplied_alpha
                        .unwrap_or(image.premultiplied_alpha()),
                );

            gpu.copy_external_image_to_texture(
                &source.into(),
                &staging_dest.into(),
                &Extent3d::new(self.width, Some(self.height), Some(1)).into(),
            )?;

            // Create command encoder for this layer's operations
            let layer_encoder = gpu.create_command_encoder(Some("Texture Pool Layer Upload"));

            // Convert sRGB to linear if needed
            let final_staging = if color.srgb_to_linear {
                convert_srgb_to_linear(
                    gpu,
                    &layer_encoder,
                    &staging_src,
                    &staging_dst,
                    self.width,
                    self.height,
                )
                .await?;
                &staging_dst
            } else {
                &staging_src
            };

            // Copy from staging to destination array layer
            let src_info = TexelCopyTextureInfo::new(final_staging).with_mip_level(0);
            let dst_info = TexelCopyTextureInfo::new(&dest_tex)
                .with_mip_level(0)
                .with_origin(Origin3d::new().with_z(index as u32));

            layer_encoder.copy_texture_to_texture(
                &src_info.into(),
                &dst_info.into(),
                &Extent3d::new(self.width, Some(self.height), Some(1)).into(),
            )?;

            // Submit this layer's operations
            let layer_buffer = layer_encoder.finish();
            gpu.submit_commands(&layer_buffer);

            // Destroy staging textures for this layer
            // GPU will keep them alive until commands complete
            staging_src.destroy();
            staging_dst.destroy();

            mipmap_texture_kinds.push(color.mipmap_kind);
        }

        if self.mipmap {
            generate_mipmaps(gpu, &dest_tex, &mipmap_texture_kinds, mipmap_levels).await?;
        }

        let dest_view = dest_tex
            .create_view_with_descriptor(
                &TextureViewDescriptor::new(Some("Texture Pool Array View"))
                    .with_dimension(TextureViewDimension::N2dArray)
                    .with_array_layer_count(layers)
                    .with_mip_level_count(mipmap_levels) // Only access mip level 0 for writing
                    .into(),
            )
            .map_err(AwsmCoreError::create_texture_view)?;

        self.gpu_dirty = false;
        self.gpu_texture = Some(dest_tex);
        self.gpu_texture_view = Some(dest_view);

        Ok(())
    }
}

#[cfg(feature = "texture-export")]
static TEXTURE_USAGE_MIPMAP: LazyLock<TextureUsage> = LazyLock::new(|| {
    TextureUsage::new()
        .with_storage_binding()
        .with_texture_binding()
        .with_copy_src()
        .with_copy_dst()
});

#[cfg(not(feature = "texture-export"))]
static TEXTURE_USAGE_MIPMAP: LazyLock<TextureUsage> = LazyLock::new(|| {
    TextureUsage::new()
        .with_storage_binding()
        .with_texture_binding()
        .with_copy_dst()
});

#[cfg(feature = "texture-export")]
static TEXTURE_USAGE_NO_MIPMAP: LazyLock<TextureUsage> = LazyLock::new(|| {
    TextureUsage::new()
        .with_storage_binding()
        .with_copy_src()
        .with_copy_dst()
});

#[cfg(not(feature = "texture-export"))]
static TEXTURE_USAGE_NO_MIPMAP: LazyLock<TextureUsage> =
    LazyLock::new(|| TextureUsage::new().with_storage_binding().with_copy_dst());
