//! Mega texture atlas packing and metadata.

pub mod mipmap;
/// A "MegaTexture" can be used to store a large number of images
/// in a way that ultimately maps to a collection of large GPU texture arrays.
///
/// It is parameterized over `ID` so that entries can be identified no matter where they are in the atlas.
///
/// It's structured as a collection of atlases, each of which contains multiple layers.
/// In other words, each atlas is a 2D texture array, and each layer is an item in that texture-array
///
/// It grows dynamically as needed, in two ways:
///
/// 1. Keep trying to add images to the current layer until it is full (fullness is max width/height)
/// 2. Keep trying to add layers to the current atlas until it is full (fullness is max depth)
/// 3. Keep trying to add atlases indefinitely (if we run out here, we're out of resources)
///
/// The limits are determined by the GPU's capabilities, such as max texture size, max texture array layers, and max buffer size.
///
/// These limits can be quite high by requesting to raise those limits from the device, which is made easy
/// by initializing the gpu builder with `DeviceRequestLimits::max_all()`
///
/// Going further would require a more complex system, such as a streaming MegaTexture
///
/// Each layer is packed using a 2D bin packing algorithm, which allows for efficient use of space.
///
/// Lastly, each original image is tracked with its 3d index into the MegaTexture (atlas, layer, entry)
/// as well as the pixel offset in the layer texture, UV offset, and UV scale.
pub mod pipeline;
#[cfg(feature = "serde")]
pub mod report;
pub mod writer;

use std::collections::HashMap;

use crate::{
    error::{AwsmCoreError, Result},
    texture::mipmap::calculate_mipmap_levels,
};
use binpack2d::{
    maxrects::{Heuristic, MaxRectsBin},
    Dimension,
};

use crate::image::ImageData;

/// Texture type for mipmap generation filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureType {
    /// Standard color/albedo textures (sRGB or linear)
    Albedo,
    /// Normal maps - require renormalization after filtering
    Normal,
    /// Metallic/roughness - roughness needs perceptual averaging (r²)
    MetallicRoughness,
    /// Occlusion maps - standard averaging
    Occlusion,
    /// Emissive textures - standard averaging
    Emissive,
}

impl Default for TextureType {
    fn default() -> Self {
        Self::Albedo
    }
}

/// Atlas-backed mega texture with dynamic growth.
pub struct MegaTexture<ID> {
    // width and height of each layer in each atlas
    pub texture_size: u32,
    // the depth of each atlas, i.e. how many layers it can have
    pub atlas_depth: u32,
    // padding around each image, useful for mipmapping and avoiding artifacts
    pub padding: u32,

    pub mipmap: bool,

    pub(super) atlases: Vec<MegaTextureAtlas<ID>>,
    // This is a lookup table for the index of each image in the mega texture
    lookup: HashMap<ID, MegaTextureIndex>,
}

/// Index into the mega texture atlas, layer, and entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MegaTextureIndex {
    pub atlas: u16,
    pub layer: u16,
    pub entry: u16,
}

impl MegaTextureIndex {
    /// Creates an index from atlas, layer, and entry indices.
    pub fn new(atlas: usize, layer: usize, entry: usize) -> Self {
        Self {
            atlas: atlas.try_into().expect("Atlas index out of bounds"),
            layer: layer.try_into().expect("Layer index out of bounds"),
            entry: entry.try_into().expect("Entry index out of bounds"),
        }
    }
}

pub(super) struct MegaTextureAtlas<ID> {
    pub layers: Vec<MegaTextureLayer<ID>>,
    pub texture_size: u32,
    pub max_depth: u32,
    pub padding: u32,
}

pub(super) struct MegaTextureLayer<ID> {
    pub entries: Vec<MegaTextureEntry<ID>>,
    pub packer: MaxRectsBin,
}

/// Single entry within a mega texture layer.
pub struct MegaTextureEntry<ID> {
    pub pixel_offset: [u32; 2],
    pub image_data: ImageData,
    pub id: ID,
    pub is_srgb_encoded: bool,
    pub texture_type: TextureType,
}

impl<ID> MegaTextureEntry<ID>
where
    ID: Clone,
{
    /// Converts to a GPU-friendly info struct.
    pub fn into_info(&self, index: MegaTextureIndex, atlas_size: u32) -> MegaTextureEntryInfo<ID> {
        let (width_u32, height_u32) = self.image_data.size();

        let height = height_u32 as f32;
        let width = width_u32 as f32;
        let x = self.pixel_offset[0] as f32;
        let y = self.pixel_offset[1] as f32;

        let atlas_dimensions = atlas_size as f32;

        let uv_offset_x = (x + 0.5) / atlas_dimensions;
        let uv_offset_y = (y + 0.5) / atlas_dimensions;

        let uv_scale_x = (width - 1.0) / atlas_dimensions;
        let uv_scale_y = (height - 1.0) / atlas_dimensions;

        let grad_scale_x = width / atlas_dimensions;
        let grad_scale_y = height / atlas_dimensions;

        MegaTextureEntryInfo {
            pixel_offset: self.pixel_offset,
            size: [width_u32, height_u32],
            index,
            id: self.id.clone(),
            uv_scale: [uv_scale_x, uv_scale_y],
            uv_offset: [uv_offset_x, uv_offset_y],
            grad_scale: [grad_scale_x, grad_scale_y],
        }
    }
}

// Does not include ImageData
/// Runtime info for a mega texture entry.
#[derive(Clone, Debug)]
pub struct MegaTextureEntryInfo<ID> {
    pub pixel_offset: [u32; 2],
    pub size: [u32; 2],
    pub index: MegaTextureIndex,
    pub id: ID,
    pub uv_scale: [f32; 2],
    pub uv_offset: [f32; 2],
    pub grad_scale: [f32; 2],
}

/// Summary information for a mega texture.
#[derive(Clone, Debug)]
pub struct MegaTextureInfo<ID>
where
    ID: Clone,
{
    entries: Vec<Vec<Vec<MegaTextureEntryInfo<ID>>>>,
    texture_size: u32,
    max_depth: u32,
    max_bindings_per_group: u32,
    max_bind_groups: u32,
    mip_levels: Option<u32>,
}

impl<ID> MegaTexture<ID>
where
    ID: std::hash::Hash + Eq + Clone + std::fmt::Debug,
{
    /// Creates a mega texture based on device limits and padding.
    pub fn new(limits: &web_sys::GpuSupportedLimits, padding: u32) -> Self {
        let (texture_size, max_depth) = max_dimensions(limits);

        Self {
            atlases: Vec::new(),
            texture_size,
            atlas_depth: max_depth,
            padding,
            mipmap: true,
            lookup: HashMap::new(),
        }
    }

    /// Looks up a stored entry index by custom ID.
    pub fn get_index(&self, custom_id: &ID) -> Option<MegaTextureIndex> {
        self.lookup.get(custom_id).cloned()
    }

    /// Returns summary info for all atlas entries.
    pub fn info(&self, limits: &web_sys::GpuSupportedLimits) -> MegaTextureInfo<ID> {
        let entries: Vec<Vec<Vec<MegaTextureEntryInfo<ID>>>> = self
            .atlases
            .iter()
            .enumerate()
            .map(|(atlas_index, atlas)| {
                atlas
                    .layers
                    .iter()
                    .enumerate()
                    .map(|(layer_index, layer)| {
                        layer
                            .entries
                            .iter()
                            .enumerate()
                            .map(|(entry_index, entry)| {
                                let index =
                                    MegaTextureIndex::new(atlas_index, layer_index, entry_index);
                                entry.into_info(index, self.texture_size)
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect();

        let mip_levels = if self.mipmap {
            Some(calculate_mipmap_levels(
                self.texture_size,
                self.texture_size,
            ))
        } else {
            None
        };

        MegaTextureInfo {
            texture_size: self.texture_size,
            max_depth: self.atlas_depth,
            entries,
            mip_levels,
            max_bindings_per_group: limits.max_sampled_textures_per_shader_stage(),
            max_bind_groups: limits.max_bind_groups(),
        }
    }

    /// Returns a stored entry by custom ID.
    pub fn get_entry(&self, custom_id: &ID) -> Option<&MegaTextureEntry<ID>> {
        self.get_index(custom_id).and_then(
            |MegaTextureIndex {
                 atlas: atlas_index,
                 layer: layer_index,
                 entry: entry_index,
             }| {
                self.atlases
                    .get(atlas_index as usize)
                    .and_then(|atlas| atlas.layers.get(layer_index as usize))
                    .and_then(|layer| layer.entries.get(entry_index as usize))
            },
        )
    }

    /// Returns entry info by custom ID.
    pub fn get_entry_info(&self, custom_id: &ID) -> Option<MegaTextureEntryInfo<ID>> {
        let index = self.get_index(custom_id)?;
        self.get_entry(custom_id)
            .map(|entry| entry.into_info(index, self.texture_size))
    }

    /// Returns the number of texture bindings needed for this mega texture.
    pub fn bindings_len(&self, limits: &web_sys::GpuSupportedLimits) -> Result<u32> {
        let max_atlases = limits.max_sampled_textures_per_shader_stage();
        let total_atlases = self.atlases.len() as u32;

        if total_atlases > max_atlases {
            Err(AwsmCoreError::MegaTextureTooManyAtlases {
                total_atlases,
                max_atlases,
            })
        } else {
            Ok(total_atlases)
        }
    }

    /// Adds images to the mega texture and returns their entry info.
    pub fn add_entries(
        &mut self,
        mut images: Vec<(ImageData, ID, IsSrgbEncoded, TextureType)>,
    ) -> Result<Vec<MegaTextureEntryInfo<ID>>> {
        if self.atlases.is_empty() {
            self.atlases.push(MegaTextureAtlas::new(
                self.texture_size,
                self.atlas_depth,
                self.padding,
            ));
        }

        let mut new_entries = Vec::new();

        let mut atlas_index = 0;

        loop {
            let rejected_images = self.atlases[atlas_index].add_entries(
                &mut self.lookup,
                atlas_index,
                images,
                &mut new_entries,
            )?;

            if rejected_images.is_empty() {
                return Ok(new_entries);
            }

            // If we got rejected images, we need to create a new atlas (all layers are full up to max depth)
            images = rejected_images;

            if atlas_index == self.atlases.len() - 1 {
                // If we are at the last atlas, we need to create a new one
                self.atlases.push(MegaTextureAtlas::new(
                    self.texture_size,
                    self.atlas_depth,
                    self.padding,
                ));
            }

            atlas_index += 1;
        }
    }

    /// Returns the number of atlases in use.
    pub fn atlases_len(&self) -> usize {
        self.atlases.len()
    }

    /// Returns the number of layers in the given atlas.
    pub fn layer_len(&self, atlas_index: usize) -> usize {
        self.atlases
            .get(atlas_index)
            .map_or(0, |atlas| atlas.layers.len())
    }

    /// Returns the mipmap level count for the atlas size.
    pub fn mipmap_levels(&self) -> u32 {
        calculate_mipmap_levels(self.texture_size, self.texture_size)
    }
}

type IsSrgbEncoded = bool;

impl<ID> MegaTextureAtlas<ID>
where
    ID: std::hash::Hash + Eq + Clone + std::fmt::Debug,
{
    /// Creates an empty atlas.
    pub fn new(texture_size: u32, max_depth: u32, padding: u32) -> Self {
        Self {
            layers: Vec::new(),
            texture_size,
            max_depth,
            padding,
        }
    }

    // return is the rejected images that could not be placed in the atlas due to max depth
    /// Adds images to this atlas and returns any rejected images.
    pub fn add_entries(
        &mut self,
        lookup: &mut HashMap<ID, MegaTextureIndex>,
        atlas_index: usize,
        images: Vec<(ImageData, ID, IsSrgbEncoded, TextureType)>,
        new_entries: &mut Vec<MegaTextureEntryInfo<ID>>,
    ) -> Result<Vec<(ImageData, ID, IsSrgbEncoded, TextureType)>> {
        if images.is_empty() {
            return Ok(images);
        }

        // allows us to have a stable index and mutable vec that we can take from
        let mut images: Vec<Option<(ImageData, ID, IsSrgbEncoded, TextureType)>> =
            images.into_iter().map(Some).collect();

        let padding = self.padding as i32;
        let padding_width_x2 = padding * 2;
        let padding_height_x2 = padding * 2;

        let mut items_to_place: Vec<Dimension> = images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                let (width, height) = image.as_ref().unwrap().0.size();
                Dimension::with_id(
                    index as isize,
                    width as i32 + padding_width_x2,
                    height as i32 + padding_height_x2,
                    0,
                )
            })
            .collect();

        if self.layers.is_empty() {
            self.layers
                .push(MegaTextureLayer::new(self.texture_size, self.texture_size));
        }

        let mut layer_index = 0;

        loop {
            let current_layer = &mut self.layers[layer_index];
            let atlas_width = self.texture_size as i32;
            let atlas_height = self.texture_size as i32;

            let (inserted, rejected) = current_layer
                .packer
                .insert_list(&items_to_place, Heuristic::BestAreaFit);

            if inserted.is_empty() && !items_to_place.is_empty() && current_layer.entries.is_empty()
            {
                let (largest_image_width, largest_image_height) =
                    items_to_place.iter().fold((0, 0), |(max_w, max_h), dim| {
                        (
                            max_w.max(dim.width() as usize),
                            max_h.max(dim.height() as usize),
                        )
                    });

                return Err(AwsmCoreError::MegaTextureAtlasSize {
                    largest_img_width: largest_image_width - (padding_width_x2 as usize),
                    largest_img_height: largest_image_height - (padding_height_x2 as usize),
                    atlas_width: atlas_width as usize,
                    atlas_height: atlas_height as usize,
                    padding: padding as usize,
                });
            }

            for rect in inserted.into_iter() {
                let (image_data, id, is_srgb_encoded, texture_type) =
                    images[rect.id() as usize].take().unwrap();

                let index = MegaTextureIndex {
                    atlas: atlas_index
                        .try_into()
                        .map_err(AwsmCoreError::MegaTextureIndexSize)?,
                    layer: layer_index
                        .try_into()
                        .map_err(AwsmCoreError::MegaTextureIndexSize)?,
                    entry: current_layer
                        .entries
                        .len()
                        .try_into()
                        .map_err(AwsmCoreError::MegaTextureIndexSize)?,
                };

                if lookup.insert(id.clone(), index).is_some() {
                    return Err(AwsmCoreError::MegaTextureDuplicateId {
                        id: format!("{id:?}"),
                    });
                }

                let pixel_offset = (rect.x() + padding, rect.y() + padding);

                let entry = MegaTextureEntry {
                    pixel_offset: [pixel_offset.0 as u32, pixel_offset.1 as u32],
                    id,
                    image_data,
                    is_srgb_encoded,
                    texture_type,
                };

                new_entries.push(entry.into_info(index, self.texture_size));

                current_layer.entries.push(entry);
            }

            if rejected.is_empty() {
                // finished!
                break;
            }

            if layer_index as u32 >= self.max_depth {
                let rejected_images: Vec<(ImageData, ID, IsSrgbEncoded, TextureType)> = rejected
                    .into_iter()
                    .filter_map(|dim| images[dim.id() as usize].take())
                    .collect();

                return Ok(rejected_images);
            }

            if layer_index == self.layers.len() - 1 {
                // If we are at the last layer, we need to create a new one
                self.layers.push(MegaTextureLayer::new(
                    atlas_width as u32,
                    atlas_height as u32,
                ));
            }

            items_to_place = rejected;

            layer_index += 1;
        }

        Ok(Vec::new())
    }
}

impl<ID> MegaTextureLayer<ID> {
    /// Creates a layer with the given size.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            entries: Vec::new(),
            packer: MaxRectsBin::new(width as i32, height as i32),
        }
    }
}

fn max_dimensions(limits: &web_sys::GpuSupportedLimits) -> (u32, u32) {
    let max_dimension_2d = limits.max_texture_dimension_2d();
    let max_depth_2d_array = limits.max_texture_array_layers();
    let max_memory = limits.max_buffer_size();

    // Rgba16Float = 4 channels * 2 bytes per channel = 8 bytes per pixel
    let bytes_per_pixel = 8u32;

    let mut texture_size = max_dimension_2d;

    loop {
        if ((texture_size * texture_size) * bytes_per_pixel) as f64 <= max_memory {
            break;
        }
        tracing::warn!(
            "Max texture size {}x{} exceeds max buffer size {}, reducing to {}x{}",
            texture_size,
            texture_size,
            max_memory,
            texture_size / 2,
            texture_size / 2
        );
        texture_size /= 2;
    }

    let memory_per_texture = texture_size * texture_size * bytes_per_pixel;
    let max_depth_by_memory = (max_memory / memory_per_texture as f64).floor() as u32;

    let max_depth = max_depth_2d_array.min(max_depth_by_memory);

    (texture_size, max_depth)
}
