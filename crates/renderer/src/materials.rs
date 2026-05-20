//! Material storage + GPU upload management.
//!
//! Material **shading models** (PBR / Unlit / Toon, the `MaterialShader`
//! trait, the WGSL fragments) live in the sibling `awsm-materials` crate and
//! are re-exported here for back-compat. This module owns the renderer-side
//! `Materials` slotmap manager, the per-material `MaterialKey`, the GPU
//! storage buffer, and the `Material` sum type the slotmap stores.

use std::sync::LazyLock;

use awsm_materials::MaterialShader;
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
    textures::{AwsmTextureError, Textures},
    AwsmRenderer, AwsmRendererLogging,
};

// Re-export the material types from `awsm-materials` so consumers can keep
// using `crate::materials::*` paths.
pub use awsm_materials::{MaterialAlphaMode, MaterialShaderId, MaterialTexture, TextureContext};

/// PBR material parameters — re-exported from `awsm-materials`.
pub mod pbr {
    pub use awsm_materials::pbr::*;
}

/// Unlit material parameters — re-exported from `awsm-materials`.
pub mod unlit {
    pub use awsm_materials::unlit::*;
}

/// Toon material parameters — re-exported from `awsm-materials`.
pub mod toon {
    pub use awsm_materials::toon::*;
}

/// Storage-buffer writer helpers — re-exported from `awsm-materials`.
pub mod writer {
    pub use awsm_materials::writer::*;
}

use awsm_materials::{pbr::PbrMaterial, toon::ToonMaterial, unlit::UnlitMaterial};

impl AwsmRenderer {
    /// Updates a material in place.
    pub fn update_material(&mut self, key: MaterialKey, f: impl FnMut(&mut Material)) {
        self.materials.update(key, &self.textures, f);
    }

    /// Removes a material and frees its slot in the materials storage
    /// buffer. Callers must ensure no live mesh still references `key`
    /// (e.g. tear down meshes first). Returns `true` if the material
    /// existed; `false` if it was already gone.
    pub fn remove_material(&mut self, key: MaterialKey) -> bool {
        self.materials.remove(key)
    }
}

/// Material variants supported by the renderer.
#[derive(Debug, Clone)]
pub enum Material {
    Pbr(Box<PbrMaterial>),
    Unlit(UnlitMaterial),
    Toon(Box<ToonMaterial>),
}

impl Material {
    /// Returns the shader-id of this material — the load-bearing
    /// dispatch key for the opaque compute pass after the shader
    /// split (Cluster 6.1 prereq). Pipelines are cached per
    /// `(MsaaConfig, mipmaps, shader_id)` so a PBR mesh and a Toon
    /// mesh in the same frame route to distinct, specialized
    /// pipelines instead of one fat shader with a runtime branch.
    pub fn shader_id(&self) -> MaterialShaderId {
        match self {
            Material::Pbr(_) => MaterialShaderId::Pbr,
            Material::Unlit(_) => MaterialShaderId::Unlit,
            Material::Toon(_) => MaterialShaderId::Toon,
        }
    }

    /// Returns true if the material renders in the transparency pass.
    pub fn is_transparency_pass(&self) -> bool {
        match self {
            Material::Pbr(m) => MaterialShader::is_transparency_pass(m.as_ref()),
            Material::Unlit(m) => MaterialShader::is_transparency_pass(m),
            Material::Toon(m) => MaterialShader::is_transparency_pass(m.as_ref()),
        }
    }

    /// Returns the alpha mask cutoff if applicable.
    pub fn alpha_mask(&self) -> Option<f32> {
        match self {
            Material::Pbr(m) => m.alpha_cutoff(),
            Material::Unlit(m) => m.alpha_cutoff(),
            Material::Toon(m) => m.alpha_cutoff(),
        }
    }

    /// Returns true if the material is flagged as double-sided. Callers
    /// that build a `Mesh` from a `MaterialKey` use this to propagate the
    /// flag onto `Mesh::double_sided`, which is what actually drives
    /// `cull_mode` at pipeline-build time.
    pub fn double_sided(&self) -> bool {
        match self {
            Material::Pbr(m) => m.double_sided(),
            Material::Unlit(m) => m.double_sided(),
            Material::Toon(m) => m.double_sided(),
        }
    }

    /// Returns true if the material implements
    /// `KHR_materials_transmission` (transmission factor > 0 or a
    /// transmission texture). Used by the transparent pipeline
    /// builder to flip on depth-write — transmissive surfaces want
    /// a single front-face fragment per pixel (so back-face refraction
    /// doesn't double-composite over front-face refraction and wipe
    /// the silhouette), while pure alpha-blend transparents want
    /// depth-write off so layered alpha (smoke through dome) composes
    /// correctly. Only PBR currently exposes the extension.
    pub fn has_transmission(&self) -> bool {
        match self {
            Material::Pbr(m) => m.has_transmission(),
            Material::Unlit(_) | Material::Toon(_) => false,
        }
    }

    /// Returns the packed uniform buffer data for the material.
    pub fn uniform_buffer_data(&self, ctx: &dyn TextureContext) -> Vec<u8> {
        let mut data = Vec::with_capacity(256);
        match self {
            Material::Pbr(m) => {
                MaterialShader::write_uniform_buffer(m.as_ref(), ctx, &mut data);
            }
            Material::Unlit(m) => {
                MaterialShader::write_uniform_buffer(m, ctx, &mut data);
            }
            Material::Toon(m) => {
                MaterialShader::write_uniform_buffer(m.as_ref(), ctx, &mut data);
            }
        }
        data
    }
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
    pub fn insert(&mut self, material: Material, textures: &Textures) -> MaterialKey {
        let is_transparency_pass = material.is_transparency_pass();

        let key = self.lookup.insert(material);
        if is_transparency_pass {
            self._is_transparency_pass.insert(key, ());
        }

        self.update(key, textures, |_| {});

        key
    }

    /// Removes a material from the slotmap + storage buffer. Returns
    /// `true` if the key existed; `false` if it was already gone.
    pub fn remove(&mut self, key: MaterialKey) -> bool {
        let removed = self.lookup.remove(key).is_some();
        if removed {
            self._is_transparency_pass.remove(key);
            self.buffer.remove(key);
            self.gpu_dirty = true;
        }
        removed
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
    /// Intentionally non-atomic: `f` mutates the stored `Material` in place
    /// and the transparency-pass classification is updated before the
    /// fallible GPU buffer write. On failure we log and leave CPU state
    /// as-is rather than rolling back. The buffer-write path is a hot path,
    /// and the error cases (GPU buffer capacity overflow) are not expected
    /// to occur in normal operation.
    pub fn update(
        &mut self,
        key: MaterialKey,
        textures: &Textures,
        mut f: impl FnMut(&mut Material),
    ) {
        if let Some(material) = self.lookup.get_mut(key) {
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

            let data = material.uniform_buffer_data(textures);
            match self.buffer.update(key, &data) {
                Ok(_) => {
                    self.gpu_dirty = true;
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to update material buffer for key {:?}: {:?}",
                        key,
                        e
                    );
                }
            }
        }
    }

    /// Returns true if the material uses the transparency pass.
    pub fn is_transparency_pass(&self, key: MaterialKey) -> bool {
        self._is_transparency_pass.contains_key(key)
    }

    /// Returns the material's `MaterialShaderId` (PBR / Unlit / Toon).
    /// The opaque compute pass routes each mesh's `effective_material_key`
    /// through this to pick the matching specialized compute pipeline.
    /// Returns `Pbr` for unknown keys — defensive default; the caller
    /// should never hit this path because the key came from a `Mesh`
    /// already validated against `Materials::insert`.
    pub fn shader_id(&self, key: MaterialKey) -> MaterialShaderId {
        self.lookup
            .get(key)
            .map(|m| m.shader_id())
            .unwrap_or(MaterialShaderId::Pbr)
    }

    /// Returns true if the material implements
    /// `KHR_materials_transmission`. See [`Material::has_transmission`]
    /// for why the transparent pipeline branches depth-write on this.
    pub fn has_transmission(&self, key: MaterialKey) -> bool {
        self.lookup
            .get(key)
            .map(|m| m.has_transmission())
            .unwrap_or(false)
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
}
