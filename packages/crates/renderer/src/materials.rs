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

/// FlipBook (sprite-sheet) material parameters — re-exported from
/// `awsm-materials`. The upstream module is itself gated by the
/// `flipbook` Cargo feature on `awsm-materials` (default-on); since
/// this crate depends on `awsm-materials` with default features, the
/// re-export is always available here.
pub mod flipbook {
    pub use awsm_materials::flipbook::*;
}

/// Storage-buffer writer helpers — re-exported from `awsm-materials`.
pub mod writer {
    pub use awsm_materials::writer::*;
}

use awsm_materials::{
    dynamic::DynamicMaterial, flipbook::FlipBookMaterial, pbr::PbrMaterial, toon::ToonMaterial,
    unlit::UnlitMaterial,
};

impl AwsmRenderer {
    /// Updates a material in place.
    pub fn update_material(&mut self, key: MaterialKey, f: impl FnMut(&mut Material)) {
        self.materials.update(
            key,
            &self.textures,
            &self.dynamic_materials,
            &self.extras_pool,
            f,
        );
        // A user edit may have changed the material's derived feature-set
        // (e.g. added a normal map) → its variant bucket may differ. Flag
        // the reconcile pass to re-resolve on the next frame.
        self.materials.mark_variants_dirty();
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
    /// Sprite-sheet flipbook. See [`awsm_materials::flipbook`] for
    /// authoring + WGSL semantics.
    FlipBook(Box<FlipBookMaterial>),
    /// Runtime-registered custom material. Backed by the generic
    /// [`DynamicMaterial`] interpreter — see
    /// [`crate::dynamic_materials`] for the registration API and the
    /// WGSL author contract.
    Custom(Box<DynamicMaterial>),
}

impl Material {
    /// Returns the shader-id of this material — the load-bearing
    /// dispatch key for the opaque compute pass. Pipelines are cached
    /// per `(MsaaConfig, mipmaps, shader_id)` so a PBR mesh and a
    /// Toon mesh in the same frame route to distinct, specialized
    /// pipelines instead of one fat shader with a runtime branch.
    pub fn shader_id(&self) -> MaterialShaderId {
        match self {
            Material::Pbr(_) => MaterialShaderId::PBR,
            Material::Unlit(_) => MaterialShaderId::UNLIT,
            Material::Toon(_) => MaterialShaderId::TOON,
            Material::FlipBook(_) => MaterialShaderId::FLIPBOOK,
            Material::Custom(m) => m.shader_id,
        }
    }

    /// Returns true if the material renders in the transparency pass.
    pub fn is_transparency_pass(&self) -> bool {
        match self {
            Material::Pbr(m) => MaterialShader::is_transparency_pass(m.as_ref()),
            Material::Unlit(m) => MaterialShader::is_transparency_pass(m),
            Material::Toon(m) => MaterialShader::is_transparency_pass(m.as_ref()),
            Material::FlipBook(m) => MaterialShader::is_transparency_pass(m.as_ref()),
            // Dynamic instances snapshot the registration's `alpha_mode` at
            // construction time (`DynamicMaterial::alpha_mode`). MASK is NOT
            // transparency — like built-in PBR (step A), a custom MASK material
            // is alpha-tested OPAQUE: its MAIN WGSL shades in the opaque compute
            // (OpaqueShadingOutput contract) and its 2nd alpha-only WGSL discards
            // cutouts in the masked visibility raster. Only BLEND routes to the
            // forward transparent pass.
            Material::Custom(m) => {
                matches!(m.alpha_mode, awsm_materials::MaterialAlphaMode::Blend)
            }
        }
    }

    /// Returns the alpha mask cutoff if applicable.
    pub fn alpha_mask(&self) -> Option<f32> {
        match self {
            Material::Pbr(m) => m.alpha_cutoff(),
            Material::Unlit(m) => m.alpha_cutoff(),
            Material::Toon(m) => m.alpha_cutoff(),
            Material::FlipBook(m) => m.alpha_cutoff(),
            Material::Custom(m) => match m.alpha_mode {
                awsm_materials::MaterialAlphaMode::Mask { cutoff } => Some(cutoff),
                _ => None,
            },
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
            Material::FlipBook(m) => m.double_sided(),
            Material::Custom(m) => m.double_sided,
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
            Material::FlipBook(_) => false,
            // Dynamic materials cannot opt into KHR_materials_transmission.
            // Reasoning: transmission samples the pre-blit opaque
            // target, which the dynamic-material wrapper intentionally
            // doesn't expose (the `frag_pos: vec4<f32>` + `Camera` args
            // that `sample_transmission_background(...)` needs aren't on
            // `TransparentShadingInput`). Materials that need refractive
            // sampling promote to first-party PBR.
            Material::Custom(_) => false,
        }
    }

    /// Returns the packed uniform buffer data for the material.
    ///
    /// `dynamic_ctx` is only consulted for [`Material::Custom`]
    /// instances; first-party variants take the simpler
    /// [`TextureContext`]-only path.
    pub fn uniform_buffer_data(
        &self,
        ctx: &dyn TextureContext,
        dynamic_ctx: &dyn awsm_materials::dynamic::DynamicMaterialContext,
    ) -> Vec<u8> {
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
            Material::FlipBook(m) => {
                MaterialShader::write_uniform_buffer(m.as_ref(), ctx, &mut data);
            }
            Material::Custom(m) => {
                // Dynamic materials walk the registry's layout via the
                // context (DynamicMaterialContext). See
                // crates/materials/src/dynamic.rs.
                m.write_uniform_buffer_with_layout(dynamic_ctx, &mut data);
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
    /// Per-material override for the payload's first u32 (the
    /// `shader_id`). The specialize-only design routes an opaque PBR/Toon
    /// material to a per-feature-set *variant* bucket whose id is
    /// registry-allocated; that variant id — not the canonical
    /// `Material::shader_id()` — is what `material_classify` routes on and
    /// what the variant's opaque pipeline guards against. The
    /// `AwsmRenderer` variant-reconcile pass resolves each material's
    /// variant and records it here; [`Self::update`] patches the first 4
    /// payload bytes with it, and [`Self::shader_id`] returns it for
    /// pipeline selection. Absent → the material uses its canonical id
    /// (Unlit/Flipbook/Custom/unreconciled).
    resolved_shader_id: SecondaryMap<MaterialKey, MaterialShaderId>,
    /// Set when a material that may need (re)routing to a feature-set
    /// variant enters or is edited; cleared by the renderer's reconcile
    /// pass. Starts `true` so the first frame reconciles.
    variants_dirty: bool,
    /// Membership set of the material keys that render in the transparency
    /// pass (Blend/Mask/transmission), kept in sync on insert/update/remove.
    /// Read by [`Self::is_transparency_pass`].
    transparency_pass_keys: SecondaryMap<MaterialKey, ()>,
    uploader: crate::buffer::mapped_uploader::MappedUploader,
    /// Sticky: set to true the first time a material implementing
    /// `KHR_materials_transmission` enters the registry, and never
    /// reset. Drives the lazy-allocation of the opaque
    /// render-texture mip chain — when this is `false`, the opaque
    /// texture is allocated with `mip_level_count = 1` (the only
    /// mip the opaque pass actually writes), saving ~33% of its
    /// allocation size. Goes true → triggers a one-time texture
    /// reallocation with the full chain next time
    /// `RenderTextures::views` runs.
    has_seen_transmission: bool,
}

impl Materials {
    /// Number of live materials (observability / leak checks).
    pub fn len(&self) -> usize {
        self.lookup.len()
    }

    /// True when no materials exist.
    pub fn is_empty(&self) -> bool {
        self.lookup.is_empty()
    }

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
            resolved_shader_id: SecondaryMap::new(),
            variants_dirty: true,
            transparency_pass_keys: SecondaryMap::new(),
            uploader: crate::buffer::mapped_uploader::MappedUploader::new("Materials"),
            has_seen_transmission: false,
        })
    }

    /// Has any material implementing `KHR_materials_transmission`
    /// entered the registry during this session? Sticky-true; used by
    /// `RenderTextures::views` to lazily grow the opaque mip chain
    /// from `mip_level_count = 1` to the full
    /// `floor(log2(max(W,H))) + 1`. Scenes that never insert a
    /// transmissive material pay 0 for the mip-chain GPU storage
    /// (~33% of the opaque texture size, a few MB on mobile / 10–20
    /// MB on desktop).
    pub fn has_seen_transmission(&self) -> bool {
        self.has_seen_transmission
    }

    /// Mapped-ring upload telemetry for this subsystem.
    pub fn upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.uploader.stats()
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
    pub fn insert(
        &mut self,
        material: Material,
        textures: &Textures,
        dynamic_materials: &crate::dynamic_materials::DynamicMaterials,
        extras_pool: &crate::dynamic_materials::extras_pool::ExtrasPool,
    ) -> MaterialKey {
        let is_transparency_pass = material.is_transparency_pass();
        // Track first transmissive-material registration so the
        // opaque texture's mip chain grows on demand instead of being
        // allocated up-front. Sticky — flipped once, never reset.
        if material.has_transmission() {
            self.has_seen_transmission = true;
        }

        let key = self.lookup.insert(material);
        if is_transparency_pass {
            self.transparency_pass_keys.insert(key, ());
        }
        // A newly-inserted material may need routing to a feature-set
        // variant bucket — flag the renderer's reconcile pass.
        self.variants_dirty = true;

        self.update(key, textures, dynamic_materials, extras_pool, |_| {});

        key
    }

    /// Returns and clears the "materials may need variant (re)routing"
    /// flag. Called once per frame by the renderer's reconcile pass.
    pub fn take_variants_dirty(&mut self) -> bool {
        std::mem::take(&mut self.variants_dirty)
    }

    /// Marks that a material was edited in a way that may change its
    /// derived feature-set (and thus its variant bucket). Drives the
    /// renderer's reconcile pass on the next frame.
    pub fn mark_variants_dirty(&mut self) {
        self.variants_dirty = true;
    }

    /// Records the resolved feature-set variant id for a material and
    /// re-packs its payload so the first u32 carries it. Called by the
    /// renderer's reconcile pass; does NOT re-flag `variants_dirty` (it
    /// is the reconcile, not a user edit).
    pub fn set_resolved_shader_id(
        &mut self,
        key: MaterialKey,
        resolved: MaterialShaderId,
        textures: &Textures,
        dynamic_materials: &crate::dynamic_materials::DynamicMaterials,
        extras_pool: &crate::dynamic_materials::extras_pool::ExtrasPool,
    ) {
        if self.resolved_shader_id.get(key) == Some(&resolved) {
            return; // unchanged — no re-pack
        }
        self.resolved_shader_id.insert(key, resolved);
        // Re-pack with the new override (the closure is a no-op; the
        // override is applied inside `update`).
        self.update(key, textures, dynamic_materials, extras_pool, |_| {});
    }

    /// Removes a material from the slotmap + storage buffer. Returns
    /// `true` if the key existed; `false` if it was already gone.
    pub fn remove(&mut self, key: MaterialKey) -> bool {
        let removed = self.lookup.remove(key).is_some();
        if removed {
            self.transparency_pass_keys.remove(key);
            self.resolved_shader_id.remove(key);
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
        dynamic_materials: &crate::dynamic_materials::DynamicMaterials,
        extras_pool: &crate::dynamic_materials::extras_pool::ExtrasPool,
        mut f: impl FnMut(&mut Material),
    ) {
        if let Some(material) = self.lookup.get_mut(key) {
            let was_transparent = material.is_transparency_pass();
            f(material);
            let is_transparent = material.is_transparency_pass();
            if was_transparent != is_transparent {
                if is_transparent {
                    self.transparency_pass_keys.insert(key, ());
                } else {
                    self.transparency_pass_keys.remove(key);
                }
            }
            // A previously-non-transmissive material can become
            // transmissive via `update_material` (e.g. authoring
            // pipeline that constructs the material first, then
            // edits in `KHR_materials_transmission` later). Sticky-true
            // so the opaque texture grows its mip chain on the next
            // `RenderTextures::views` — otherwise the transparent
            // shader's `textureNumLevels(opaque_tex)`-based
            // transmission-blur sampling reads from a 1-mip chain
            // and silently breaks transmission. The insert path
            // already does this check (`insert(...)` above); this
            // closes the post-insert-mutation gap.
            if material.has_transmission() {
                self.has_seen_transmission = true;
            }

            let dynamic_ctx =
                crate::dynamic_materials::DynamicMaterialPackContext::new(dynamic_materials)
                    .with_textures(textures)
                    .with_extras(extras_pool);
            let mut data = material.uniform_buffer_data(textures, &dynamic_ctx);
            // Patch the payload's first u32 (the shader_id) with the
            // resolved variant id when the reconcile pass has routed this
            // material to a feature-set bucket. `write_uniform_buffer`
            // writes `Material::shader_id()` (the canonical id) there in
            // little-endian; the classify pass + the variant's opaque
            // pipeline both key on this word, so it must be the variant id.
            if let Some(resolved) = self.resolved_shader_id.get(key) {
                if data.len() >= 4 {
                    data[0..4].copy_from_slice(&resolved.as_u32().to_le_bytes());
                }
            }
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
        self.transparency_pass_keys.contains_key(key)
    }

    /// Returns the material's `MaterialShaderId` (PBR / Unlit / Toon).
    /// `collect_renderables` passes each mesh's authored `material_key`
    /// through this to pick the matching specialized compute pipeline.
    /// A cheap-material LOD path historically routed through the
    /// *effective* key here; that's parked until the cheap material's
    /// offset is also plumbed into `MaterialMeshMeta`, otherwise the
    /// pipeline wouldn't match the data the shader reads.
    /// Returns `Pbr` for unknown keys — defensive default; the caller
    /// should never hit this path because the key came from a `Mesh`
    /// already validated against `Materials::insert`.
    pub fn shader_id(&self, key: MaterialKey) -> MaterialShaderId {
        // A resolved feature-set variant id (set by the reconcile pass)
        // wins over the material's canonical id — it's what classify
        // routes on and what the specialized opaque pipeline guards.
        if let Some(resolved) = self.resolved_shader_id.get(key) {
            return *resolved;
        }
        self.lookup
            .get(key)
            .map(|m| m.shader_id())
            .unwrap_or(MaterialShaderId::PBR)
    }

    /// Returns the material's **canonical** shader id (PBR / Unlit / Toon /
    /// FlipBook / a custom material's own id), ignoring any resolved
    /// feature-set variant id. The masked (alpha-tested) geometry variant keys
    /// on this: its fragment only reads base-color alpha (for built-ins) or the
    /// custom alpha-only WGSL — neither depends on a PBR feature-set variant, so
    /// one masked pipeline per *canonical* id serves every variant. Returns
    /// `Pbr` for unknown keys (defensive).
    pub fn canonical_shader_id(&self, key: MaterialKey) -> MaterialShaderId {
        self.lookup
            .get(key)
            .map(|m| m.shader_id())
            .unwrap_or(MaterialShaderId::PBR)
    }

    /// Returns the material's alpha-mask cutoff when it's a glTF `MASK`
    /// material, else `None`. Drives two things: (1) routing — a `Some`
    /// material is alpha-tested-opaque, so it renders through the masked
    /// geometry variant; (2) `MaterialMeshMeta` packing, which writes the
    /// cutoff per-mesh so the masked raster can `discard` below it.
    /// Returns `None` for unknown keys (defensive — the caller's key came
    /// from a validated `Mesh`).
    pub fn alpha_cutoff(&self, key: MaterialKey) -> Option<f32> {
        self.lookup.get(key).and_then(|m| m.alpha_mask())
    }

    /// Iterates `(key, &Material)` for materials that may route to a
    /// first-party feature-set variant (opaque PBR/Toon). Used by the
    /// renderer's reconcile pass to derive each one's feature mask.
    pub fn iter_for_variant_reconcile(&self) -> impl Iterator<Item = (MaterialKey, &Material)> {
        self.lookup
            .iter()
            .filter(|(_, m)| !m.is_transparency_pass())
    }

    /// Returns the `(ShadingBase, pbr_features)` for a material — the
    /// compile-time specialization key for its TRANSPARENT pipeline (the
    /// transparent fragment selects its body on `base` and gates PBR
    /// features, instead of a runtime `shader_id ==` uber branch). Unknown
    /// keys / non-PBR families report an inert empty mask (their bodies
    /// don't read `pbr_features`). PBR's mask is derived from the
    /// material's actual Option fields.
    pub fn transparent_variant(
        &self,
        key: MaterialKey,
    ) -> (crate::dynamic_materials::ShadingBase, u32) {
        use crate::dynamic_materials::ShadingBase;
        match self.lookup.get(key) {
            Some(Material::Pbr(m)) => (
                ShadingBase::Pbr,
                awsm_materials::pbr::PbrFeatures::from_material(m).bits(),
            ),
            Some(Material::Toon(_)) => (ShadingBase::Toon, 0),
            Some(Material::Unlit(_)) => (ShadingBase::Unlit, 0),
            Some(Material::FlipBook(_)) => (ShadingBase::Flipbook, 0),
            Some(Material::Custom(_)) | None => (ShadingBase::Custom, 0),
        }
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

    /// Returns true if a transparent-pass material should write depth.
    ///
    /// Two material classes behave *opaquely per pixel* even though they
    /// render in the transparency pass, and both need depth-write ON:
    ///
    ///   - Transmissive (`KHR_materials_transmission`) — a single
    ///     front-face fragment per pixel (see [`Material::has_transmission`]).
    ///   - Alpha-masked / cutout (`alphaMode = MASK`) — each fragment is
    ///     either fully opaque or discarded, so masked surfaces must
    ///     occlude one another via the depth buffer. Without depth-write,
    ///     interpenetrating cutout geometry (e.g. double-sided foliage)
    ///     relies solely on the per-primitive back-to-front sort and the
    ///     leaves "pop" through each other as the camera orbits.
    ///
    /// Pure alpha-*blend* surfaces (smoke, dome panes) deliberately keep
    /// depth-write OFF so layered transparents compose under the sort.
    pub fn transparent_writes_depth(&self, key: MaterialKey) -> bool {
        self.lookup
            .get(key)
            .map(|m| m.has_transmission() || m.alpha_mask().is_some())
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
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
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
                self.uploader.write_dirty_ranges(
                    gpu,
                    &self.gpu_buffer,
                    self.buffer.raw_slice().len(),
                    self.buffer.raw_slice(),
                    &ranges,
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
