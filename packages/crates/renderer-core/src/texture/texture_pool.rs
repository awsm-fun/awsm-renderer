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
        convert_srgb::{self, convert_srgb_to_linear},
        mipmap::{self, calculate_mipmap_levels, generate_mipmaps, MipmapTextureKind},
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
    /// Block-compressed array (BC/ETC2/ASTC): layers upload verbatim via
    /// `writeTexture` with their pre-supplied mip chains — no staging
    /// textures, no `srgb_to_linear` compute, no compute mip-gen (all
    /// invalid on compressed formats).
    pub compressed: bool,
    pub images: Vec<(ID, ImageData, TextureColorInfo)>,
    pub gpu_dirty: bool,
    pub gpu_texture: Option<web_sys::GpuTexture>,
    pub gpu_texture_view: Option<web_sys::GpuTextureView>,
}

/// The upload helper pipelines a pool needs — see
/// [`TexturePool::upload_pipeline_needs`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct UploadPipelineNeeds {
    /// Formats that need the sRGB → linear conversion pipeline.
    pub srgb: Vec<TextureFormat>,
    /// `(format, is_array)` pairs that need a mipmap pipeline.
    pub mipmap: Vec<(TextureFormat, bool)>,
}

impl UploadPipelineNeeds {
    /// Starts compiling every pipeline listed, in the background, and
    /// returns at once. Whoever needs one later — [`TexturePool::write_gpu`]
    /// — finds it compiled, or joins the compile still running, instead of
    /// starting it then and waiting for all of it. A failure is not reported
    /// here: the upload that needs the pipeline compiles it again and reports
    /// its own error.
    pub fn compile_in_background(self, gpu: &AwsmRendererWebGpu) {
        for format in self.srgb {
            let gpu = gpu.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = convert_srgb::prewarm_pipeline(&gpu, format).await;
            });
        }
        for (format, is_array) in self.mipmap {
            let gpu = gpu.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = mipmap::prewarm_pipeline(&gpu, format, is_array).await;
            });
        }
    }
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
    /// Pre-supplied mip level count for compressed images (they carry their
    /// whole chain and can't share an array with a different-length chain);
    /// 0 for uncompressed images, whose mips are computed per-array.
    pub compressed_mip_levels: u32,
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

        // Compressed images bucket by their pre-supplied mip chain length
        // too — arrays are created with a fixed mip_level_count, so a 1-level
        // (no-mip) image can never share one with a full-chain image.
        let compressed_mip_levels = image
            .as_compressed()
            .map(|c| c.levels.len() as u32)
            .unwrap_or(0);

        let array_key = TexturePoolArrayKey {
            width,
            height,
            format: format.into(),
            compressed_mip_levels,
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
        self.prewarm_upload_pipelines(gpu).await?;

        let mut any_dirty = false;
        for array in self.arrays.values_mut() {
            any_dirty |= array.gpu_dirty;
            array.write_gpu(gpu).await?;
        }

        Ok(any_dirty)
    }

    /// Compile, TOGETHER, every sRGB-conversion and mipmap pipeline the dirty
    /// arrays are about to use. Each is otherwise created lazily at its first
    /// use inside the per-array upload — the sRGB one while recording the
    /// first layer, the mipmap one only after that array's upload was
    /// submitted — so the first upload that needs both paid for two compiles
    /// back to back. A no-op once they exist (and for compressed arrays, which
    /// use neither).
    async fn prewarm_upload_pipelines(&self, gpu: &AwsmRendererWebGpu) -> Result<()> {
        let needs = self.upload_pipeline_needs();
        if needs.srgb.len() + needs.mipmap.len() < 2 {
            // Zero or one pipeline: nothing to overlap; the lazy path covers it.
            return Ok(());
        }
        let srgb = futures::future::try_join_all(
            needs
                .srgb
                .into_iter()
                .map(|format| convert_srgb::prewarm_pipeline(gpu, format)),
        );
        let mipmap = futures::future::try_join_all(
            needs
                .mipmap
                .into_iter()
                .map(|(format, is_array)| mipmap::prewarm_pipeline(gpu, format, is_array)),
        );
        futures::future::try_join(srgb, mipmap).await?;
        Ok(())
    }

    /// The helper pipelines the next [`Self::write_gpu`] will use for the
    /// arrays that are dirty right now: an sRGB-conversion pipeline per format
    /// that has an sRGB image, and a mipmap pipeline per `(format, is_array)`
    /// that generates mips. Compressed arrays use neither.
    ///
    /// `is_array` follows the array's CURRENT layer count, so an array still
    /// being filled reports the single-layer pipeline after its first image
    /// and the array one from its second.
    pub fn upload_pipeline_needs(&self) -> UploadPipelineNeeds {
        let mut needs = UploadPipelineNeeds::default();
        for array in self.arrays.values() {
            if !array.gpu_dirty || array.compressed {
                continue;
            }
            if array
                .images
                .iter()
                .any(|(_, _, color)| color.srgb_to_linear)
                && !needs.srgb.contains(&array.format)
            {
                needs.srgb.push(array.format);
            }
            let is_array = array.images.len() > 1;
            if array.mipmap
                && array.mipmap_levels() >= 2
                && !needs.mipmap.contains(&(array.format, is_array))
            {
                needs.mipmap.push((array.format, is_array));
            }
        }
        needs
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
        let compressed = crate::texture::block_format::is_block_compressed(format);
        Self {
            format,
            width,
            height,
            // Compressed layers arrive with their whole mip chain; there is
            // no compute mip-gen for block formats.
            mipmap: !compressed,
            compressed,
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
        if self.compressed {
            // Pre-supplied chain length (uniform across the array — the pool
            // buckets compressed images by chain length).
            self.images
                .iter()
                .find_map(|(_, image, _)| image.as_compressed())
                .map(|c| c.levels.len() as u32)
                .unwrap_or(1)
        } else if self.mipmap {
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

        if self.compressed {
            return self.write_gpu_compressed(gpu);
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

        // Single command encoder for the whole upload. Previously we
        // submitted one command buffer per layer (N round-trips for N
        // textures); batching them collapses the GPU sync overhead to
        // a single queue submit while still preserving per-layer
        // staging textures (no race risk — each layer has its own
        // src/dst pair). The `copy_external_image_to_texture` calls
        // are queue-level operations and serialize on the GPU queue
        // ahead of this command buffer.
        let upload_encoder = gpu.create_command_encoder(Some("Texture Pool Upload"));
        let mut staging_textures: Vec<web_sys::GpuTexture> =
            Vec::with_capacity(self.images.len() * 2);

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

            // Copy external image to staging_src (this is a queue operation
            // that runs ahead of the encoder we're recording below).
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

            // Optional sRGB→linear compute pass, recorded into the shared encoder.
            let final_staging = if color.srgb_to_linear {
                convert_srgb_to_linear(
                    gpu,
                    &upload_encoder,
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

            upload_encoder.copy_texture_to_texture(
                &src_info.into(),
                &dst_info.into(),
                &Extent3d::new(self.width, Some(self.height), Some(1)).into(),
            )?;

            // Stash the staging textures so we can release them once the
            // batched submit has actually been queued.
            staging_textures.push(staging_src);
            staging_textures.push(staging_dst);

            mipmap_texture_kinds.push(color.mipmap_kind);
        }

        // One submit for every layer's compute + copy work.
        let upload_buffer = upload_encoder.finish();
        gpu.submit_commands(&upload_buffer);

        // Staging textures are safe to destroy after submit — the GPU
        // keeps the underlying allocations alive until the queued
        // commands finish.
        for staging in staging_textures {
            staging.destroy();
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

    /// Compressed-array upload: every layer's pre-supplied mip chain goes in
    /// verbatim via `writeTexture` (a queue op — no encoder, no staging
    /// textures, no external-image copy). The sRGB decode rides in the
    /// `*UnormSrgb` texture format, and mips were supplied by the
    /// transcoder/container, so neither compute pass applies.
    fn write_gpu_compressed(&mut self, gpu: &AwsmRendererWebGpu) -> Result<()> {
        let mipmap_levels = self.mipmap_levels();
        let layers = self.images.len() as u32;

        let dest_tex = gpu.create_texture(
            &TextureDescriptor::new(
                self.format,
                Extent3d::new(self.width, Some(self.height), Some(layers)),
                TEXTURE_USAGE_COMPRESSED.clone(),
            )
            .with_label("Texture Pool Array Dest (compressed)")
            .with_mip_level_count(mipmap_levels)
            .with_dimension(TextureDimension::N2d)
            .into(),
        )?;

        for (index, (_, image, color)) in self.images.iter().enumerate() {
            let compressed = image.as_compressed().ok_or_else(|| {
                AwsmCoreError::CompressedImage(
                    "non-compressed image in a block-format pool array".to_string(),
                )
            })?;
            compressed.validate()?;
            if color.srgb_to_linear {
                // Not an error — but the flag is meaningless here and the
                // compute pass will NOT run; sRGB-ness must come from the
                // *UnormSrgb block format chosen at transcode time.
                tracing::warn!(
                    "srgb_to_linear requested for a compressed texture ({:?}); ignored — use an sRGB block format instead",
                    self.format
                );
            }
            compressed.write_to_texture_layer(gpu, &dest_tex, index as u32)?;
        }

        let dest_view = dest_tex
            .create_view_with_descriptor(
                &TextureViewDescriptor::new(Some("Texture Pool Array View (compressed)"))
                    .with_dimension(TextureViewDimension::N2dArray)
                    .with_array_layer_count(layers)
                    .with_mip_level_count(mipmap_levels)
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

// Block-compressed formats support neither STORAGE_BINDING nor
// RENDER_ATTACHMENT — sampling + upload only (plus COPY_SRC so the
// texture-export/readback tooling can verify what actually landed on-GPU).
#[cfg(feature = "texture-export")]
static TEXTURE_USAGE_COMPRESSED: LazyLock<TextureUsage> = LazyLock::new(|| {
    TextureUsage::new()
        .with_texture_binding()
        .with_copy_src()
        .with_copy_dst()
});

#[cfg(not(feature = "texture-export"))]
static TEXTURE_USAGE_COMPRESSED: LazyLock<TextureUsage> =
    LazyLock::new(|| TextureUsage::new().with_texture_binding().with_copy_dst());
