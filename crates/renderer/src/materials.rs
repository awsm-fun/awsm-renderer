//! Material definitions and GPU uploads.

use std::sync::LazyLock;

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};
use slotmap::{new_key_type, SecondaryMap, SlotMap};
use thiserror::Error;

use crate::{
    bind_groups::{AwsmBindGroupError, BindGroupCreate, BindGroups},
    buffer::dynamic_storage::DynamicStorageBuffer,
    buffer::helpers::write_buffer_with_dirty_ranges,
    materials::{pbr::PbrMaterial, unlit::UnlitMaterial},
    textures::{AwsmTextureError, SamplerKey, TextureKey, TextureTransformKey, Textures},
    AwsmRenderer, AwsmRendererLogging,
};

pub mod pbr;
pub mod unlit;
pub mod writer;

impl AwsmRenderer {
    /// Updates a material in place.
    pub fn update_material(
        &mut self,
        key: MaterialKey,
        f: impl FnMut(&mut Material),
    ) -> Result<()> {
        self.materials.update(key, &self.textures, f)
    }
}

/// Material variants supported by the renderer.
#[derive(Debug, Clone)]
pub enum Material {
    Pbr(Box<PbrMaterial>),
    Unlit(UnlitMaterial),
}

impl Material {
    // this should match `mesh_buffer_geometry_kind()`
    /// Returns true if the material renders in the transparency pass.
    pub fn is_transparency_pass(&self) -> bool {
        match self {
            Material::Pbr(pbr_material) => pbr_material.is_transparency_pass(),
            Material::Unlit(pbr_material) => pbr_material.is_transparency_pass(),
        }
    }

    /// Returns the alpha mask cutoff if applicable.
    pub fn alpha_mask(&self) -> Option<f32> {
        match self {
            Material::Pbr(pbr_material) => pbr_material.alpha_mask(),
            Material::Unlit(pbr_material) => pbr_material.alpha_mask(),
        }
    }

    /// Returns the packed uniform buffer data for the material.
    pub fn uniform_buffer_data(&self, textures: &Textures) -> Result<Vec<u8>> {
        match self {
            Material::Pbr(pbr_material) => {
                let data = pbr_material.uniform_buffer_data(textures)?;

                Ok(data)
            }
            Material::Unlit(unlit_material) => unlit_material.uniform_buffer_data(textures),
        }
    }
}

/// Material shader identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MaterialShaderId {
    Pbr = 1,
    Unlit = 2,
    // Toon = 3, etc.
}

const INITIAL_SIZE: usize = 8192; //Why not
static BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_copy_dst().with_storage());

/// Material storage and GPU buffer manager.
pub struct Materials {
    pub(crate) gpu_buffer: web_sys::GpuBuffer,
    lookup: SlotMap<MaterialKey, Material>,
    buffer: DynamicStorageBuffer<MaterialKey>,
    gpu_dirty: bool,
    _is_transparency_pass: SecondaryMap<MaterialKey, ()>,
}

impl Materials {
    /// Creates material storage and GPU buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(Some("Materials"), INITIAL_SIZE, *BUFFER_USAGE).into(),
        )?;

        let buffer = DynamicStorageBuffer::new(INITIAL_SIZE, Some("Materials".to_string()));

        Ok(Materials {
            lookup: SlotMap::with_key(),
            gpu_buffer,
            buffer,
            gpu_dirty: true,
            _is_transparency_pass: SecondaryMap::new(),
        })
    }

    /// Iterates over material keys.
    pub fn keys(&self) -> impl Iterator<Item = MaterialKey> + '_ {
        self.lookup.keys()
    }

    /// Iterates over materials.
    pub fn iter(&self) -> impl Iterator<Item = (MaterialKey, &Material)> {
        self.lookup.iter()
    }

    /// Returns a material by key.
    pub fn get(&self, key: MaterialKey) -> Result<&Material> {
        self.lookup.get(key).ok_or(AwsmMaterialError::NotFound(key))
    }

    /// Inserts a material and returns its key.
    pub fn insert(&mut self, material: Material, textures: &Textures) -> Result<MaterialKey> {
        let is_transparency_pass = material.is_transparency_pass();

        let key = self.lookup.insert(material);
        if is_transparency_pass {
            self._is_transparency_pass.insert(key, ());
        }

        if let Err(e) = self.update(key, textures, |_| {}) {
            self.lookup.remove(key);
            self._is_transparency_pass.remove(key);
            return Err(e);
        }

        Ok(key)
    }

    /// Returns the GPU buffer offset for a material.
    pub fn buffer_offset(&self, key: MaterialKey) -> Result<usize> {
        let offset = self
            .buffer
            .offset(key)
            .ok_or(AwsmMaterialError::BufferSlotMissing(key))?;

        #[cfg(debug_assertions)]
        {
            let max: usize = f32::MAX.to_bits() as usize;
            if offset >= max {
                tracing::error!(
                    "[material] material buffer offset {} exceeds f32 max {} - see note in material compute shader",
                    offset, max
                );
            }
        }

        Ok(offset)
    }

    /// Updates a material and refreshes GPU data.
    ///
    /// Returns `Ok(())` if the material was updated (or the key did not exist),
    /// and `Err(_)` if producing the uniform data or writing to the buffer failed.
    /// On error, the transparency-pass classification is rolled back so CPU and GPU
    /// state stay consistent.
    pub fn update(
        &mut self,
        key: MaterialKey,
        textures: &Textures,
        mut f: impl FnMut(&mut Material),
    ) -> Result<()> {
        let Some(material) = self.lookup.get_mut(key) else {
            return Ok(());
        };

        let old_is_transparency_pass = material.is_transparency_pass();
        f(material);
        let new_is_transparency_pass = material.is_transparency_pass();
        if old_is_transparency_pass != new_is_transparency_pass {
            if new_is_transparency_pass {
                self._is_transparency_pass.insert(key, ());
            } else {
                self._is_transparency_pass.remove(key);
            }
        }

        let rollback_transparency = |this: &mut Self| {
            if old_is_transparency_pass != new_is_transparency_pass {
                if old_is_transparency_pass {
                    this._is_transparency_pass.insert(key, ());
                } else {
                    this._is_transparency_pass.remove(key);
                }
            }
        };

        let data = match material.uniform_buffer_data(textures) {
            Ok(data) => data,
            Err(e) => {
                rollback_transparency(self);
                return Err(e);
            }
        };

        if let Err(e) = self.buffer.update(key, &data) {
            rollback_transparency(self);
            return Err(AwsmMaterialError::BufferCapacityOverflow(format!(
                "material uniform: {e}"
            )));
        }

        self.gpu_dirty = true;
        Ok(())
    }

    /// Returns true if the material uses the transparency pass.
    pub fn is_transparency_pass(&self, key: MaterialKey) -> bool {
        self._is_transparency_pass.contains_key(key)
    }

    /// Writes material data to the GPU.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        if self.gpu_dirty {
            let _maybe_span_guard = if logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Material Buffer GPU write").entered())
            } else {
                None
            };

            let mut resized = false;
            if let Some(new_size) = self.buffer.take_gpu_needs_resize() {
                self.gpu_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(Some("Material"), new_size, *BUFFER_USAGE).into(),
                )?;

                bind_groups.mark_create(BindGroupCreate::MaterialResize);
                resized = true;
            }

            if resized {
                self.buffer.clear_dirty_ranges();
                gpu.write_buffer(&self.gpu_buffer, None, self.buffer.raw_slice(), None, None)?;
            } else {
                let ranges = self.buffer.take_dirty_ranges();
                write_buffer_with_dirty_ranges(
                    gpu,
                    &self.gpu_buffer,
                    self.buffer.raw_slice(),
                    ranges,
                )?;
            }

            self.gpu_dirty = false;
        }
        Ok(())
    }
}

/// Texture reference used by materials.
#[derive(Clone, Debug)]
pub struct MaterialTexture {
    pub key: TextureKey,
    pub sampler_key: Option<SamplerKey>,
    pub uv_index: Option<u32>,
    pub transform_key: Option<TextureTransformKey>,
}

/// Alpha mode for materials.
#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub enum MaterialAlphaMode {
    #[default]
    Opaque,
    Mask {
        cutoff: f32,
    },
    Blend,
}

impl MaterialAlphaMode {
    /// Returns the numeric shader variant value.
    pub fn variant_as_u32(&self) -> u32 {
        match self {
            Self::Opaque => 0,
            Self::Mask { .. } => 1,
            Self::Blend => 2,
        }
    }
}

new_key_type! {
    /// Opaque key for materials.
    pub struct MaterialKey;
}

/// Result type for material operations.
pub type Result<T> = std::result::Result<T, AwsmMaterialError>;

/// Material-related errors.
#[derive(Error, Debug)]
pub enum AwsmMaterialError {
    #[error("[material] not found: {0:?}")]
    NotFound(MaterialKey),
    #[error("[material] missing alpha blend lookup: {0:?}")]
    MissingAlphaBlendLookup(MaterialKey),

    #[error("[material] missing alpha cutoff lookup: {0:?}")]
    MissingAlphaCutoffLookup(MaterialKey),

    #[error("[material] create texture view: {0}")]
    CreateTextureView(String),

    #[error("[material] unable to create bind group: {0:?}")]
    MaterialBindGroup(AwsmBindGroupError),

    #[error("[material] unable to create bind group layout: {0:?}")]
    MaterialBindGroupLayout(AwsmBindGroupError),

    #[error("[material] unable to set alpha cutoff, alpha mode is {0:?}")]
    InvalidAlphaModeForCutoff(MaterialAlphaMode),

    #[error("[material] pbr unable to resize bind group: {0:?}")]
    PbrMaterialBindGroupResize(AwsmBindGroupError),

    #[error("[material] pbr unable to write bind group: {0:?}")]
    PbrMaterialBindGroupWrite(AwsmBindGroupError),

    #[error("[material] {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[material] {0:?}")]
    Texture(#[from] AwsmTextureError),

    #[error("[material] buffer slot missing {0:?}")]
    BufferSlotMissing(MaterialKey),

    #[error("[material] buffer capacity overflow: {0}")]
    BufferCapacityOverflow(String),
}
