//! Morph target storage and GPU updates.

use std::sync::LazyLock;

use awsm_renderer_core::buffers::{BufferDescriptor, BufferUsage};
use awsm_renderer_core::renderer::AwsmRendererWebGpu;
use slotmap::{new_key_type, SlotMap};

use super::error::{AwsmMeshError, Result};
use crate::bind_groups::BindGroupCreate;
use crate::bind_groups::BindGroups;
use crate::buffer::dynamic_storage::DynamicStorageBuffer;
use crate::meshes::buffer_info::{MeshBufferGeometryMorphInfo, MeshBufferMaterialMorphInfo};
use crate::AwsmRendererLogging;

// The weights are dynamic and updated on a per-mesh basis as frequently as needed
// The values are essentially static, but may be sourced from different (large) buffers
// e.g. they are loaded up front per-gltf file
/// Geometry and material morph target storage.
pub struct Morphs {
    pub geometry: MorphData<GeometryMorphKey, MeshBufferGeometryMorphInfo>,
    pub material: MorphData<MaterialMorphKey, MeshBufferMaterialMorphInfo>,
}

impl Morphs {
    /// Creates morph target buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        Ok(Self {
            geometry: MorphData::new(gpu)?,
            material: MorphData::new(gpu)?,
        })
    }

    /// Writes morph target data to the GPU.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        // Scenes without morph targets — the common case — pay nothing
        // beyond this two-load comparison. Skipping the inner
        // `write_gpu` calls avoids the per-frame initial-dirty
        // `writeBuffer` of the empty 4 kB starter buffer on the first
        // frame after construction, plus any future change to
        // `*_dirty` defaults can't accidentally trigger an empty
        // upload mid-session.
        if self.geometry.is_empty() && self.material.is_empty() {
            return Ok(());
        }
        self.geometry.write_gpu(
            logging,
            gpu,
            bind_groups,
            BindGroupCreate::GeometryMorphTargetWeightsResize,
            BindGroupCreate::GeometryMorphTargetValuesResize,
        )?;
        self.material.write_gpu(
            logging,
            gpu,
            bind_groups,
            BindGroupCreate::MaterialMorphTargetWeightsResize,
            BindGroupCreate::MaterialMorphTargetValuesResize,
        )?;
        Ok(())
    }
}

/// Trait for morph info metadata.
pub trait MorphInfo: Clone {
    fn targets_len(&self) -> usize;
}

impl MorphInfo for MeshBufferGeometryMorphInfo {
    fn targets_len(&self) -> usize {
        self.targets_len
    }
}

impl MorphInfo for MeshBufferMaterialMorphInfo {
    fn targets_len(&self) -> usize {
        self.targets_len
    }
}

static BUFFER_USAGE_WEIGHTS: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_storage().with_copy_dst());
static BUFFER_USAGE_VALUES: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_storage().with_copy_dst());

impl<Key: slotmap::Key, Info: MorphInfo> MorphData<Key, Info> {
    /// Initial size for morph weights buffer.
    pub const WEIGHTS_INITIAL_SIZE: usize = 4096; // 4kB is a good starting point
    /// Initial size for morph values buffer.
    pub const VALUES_INITIAL_SIZE: usize = 4096; // 4kB is a good starting point

    /// Creates morph data buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let gpu_buffer_weights = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Morph Weights"),
                Self::WEIGHTS_INITIAL_SIZE,
                *BUFFER_USAGE_WEIGHTS,
            )
            .into(),
        )?;

        let gpu_buffer_values = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Morph Values"),
                Self::VALUES_INITIAL_SIZE,
                *BUFFER_USAGE_VALUES,
            )
            .into(),
        )?;

        Ok(Self {
            weights: DynamicStorageBuffer::new(
                Self::WEIGHTS_INITIAL_SIZE,
                Some("MorphWeights".to_string()),
            ),
            values: DynamicStorageBuffer::new(
                Self::VALUES_INITIAL_SIZE,
                Some("MorphValues".to_string()),
            ),
            weights_dirty: true,
            values_dirty: true,
            infos: SlotMap::with_key(),
            gpu_buffer_weights,
            gpu_buffer_values,
            weights_uploader: crate::buffer::mapped_uploader::MappedUploader::new("Morph Weights"),
            values_uploader: crate::buffer::mapped_uploader::MappedUploader::new("Morph Values"),
        })
    }

    /// Mapped-ring upload telemetry for the morph weights buffer.
    pub fn weights_upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.weights_uploader.stats()
    }

    /// Mapped-ring upload telemetry for the morph values buffer.
    pub fn values_upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.values_uploader.stats()
    }

    /// Returns morph info by key.
    pub fn get_info(&self, key: Key) -> Result<&Info> {
        self.infos
            .get(key)
            .ok_or_else(|| AwsmMeshError::MorphNotFound(format!("{:?}", key)))
    }

    /// Returns `true` when no morph data is registered. Cheap (one
    /// SlotMap length read); the morph `write_gpu` path uses this to
    /// skip its inner `geometry` + `material` writes entirely on
    /// scenes with no morph targets.
    pub fn is_empty(&self) -> bool {
        self.infos.is_empty()
    }

    /// Inserts morph data from f32 weights and values.
    pub fn insert(
        &mut self,
        morph_buffer_info: Info,
        weights: &[f32],
        values: &[f32],
    ) -> Result<Key> {
        let weights_u8 =
            unsafe { std::slice::from_raw_parts(weights.as_ptr() as *const u8, weights.len() * 4) };
        let values_u8 =
            unsafe { std::slice::from_raw_parts(weights.as_ptr() as *const u8, values.len() * 4) };

        self.insert_raw(morph_buffer_info, weights_u8, values_u8)
    }

    /// Inserts morph data from raw bytes.
    pub fn insert_raw(
        &mut self,
        morph_buffer_info: Info,
        weights: &[u8],
        values: &[u8],
    ) -> Result<Key> {
        if weights.len() / 4 != morph_buffer_info.targets_len() {
            return Err(AwsmMeshError::MorphWeightsTargetsMismatch {
                weights: weights.len(),
                targets: morph_buffer_info.targets_len(),
            });
        }

        let key = self.infos.insert(morph_buffer_info.clone());

        if let Err(e) = self
            .weights
            .update(key, weights)
            .map_err(|e| AwsmMeshError::BufferCapacityOverflow(format!("morph weights: {e}")))
            .and_then(|_| {
                self.values.update(key, values).map_err(|e| {
                    AwsmMeshError::BufferCapacityOverflow(format!("morph values: {e}"))
                })
            })
        {
            // Roll back the info entry (and any successful weights write) so we don't
            // leak an orphaned key when a buffer update fails.
            self.weights.remove(key);
            self.values.remove(key);
            self.infos.remove(key);
            return Err(e);
        }

        self.weights_dirty = true;
        self.values_dirty = true;

        Ok(key)
    }

    /// Removes morph data by key.
    pub fn remove(&mut self, key: Key) {
        self.weights.remove(key);
        self.values.remove(key);
        self.infos.remove(key);

        self.weights_dirty = true;
        self.values_dirty = true;
    }

    /// Returns the weights buffer offset for a morph key.
    pub fn weights_buffer_offset(&self, key: Key) -> Result<usize> {
        self.weights
            .offset(key)
            .ok_or_else(|| AwsmMeshError::MorphNotFound(format!("{:?}", key)))
    }

    /// Returns the values buffer offset for a morph key.
    pub fn values_buffer_offset(&self, key: Key) -> Result<usize> {
        self.values
            .offset(key)
            .ok_or_else(|| AwsmMeshError::MorphNotFound(format!("{:?}", key)))
    }

    /// Reads the current morph weights into a freshly-allocated `Vec<f32>`.
    /// Mirrors the slice layout that [`Self::update_morph_weights_with`]
    /// writes: the leading `targets_len` count word is skipped, so the
    /// returned vector holds exactly `targets_len()` weights. Returns the
    /// morph error if the key is unknown.
    pub fn read_morph_weights(&self, key: Key) -> Result<Vec<f32>> {
        let len = self.get_info(key).map(|info| info.targets_len())?;

        let slice_u8 = self
            .weights
            .get(key)
            .ok_or_else(|| AwsmMeshError::MorphNotFound(format!("{:?}", key)))?;

        // The buffer holds [count: f32, weight_0, .. weight_{len-1}] — read the
        // `len` weights after the leading count word. Decode each f32 from its
        // four little-endian bytes: the `u8` slice carries no `f32` alignment
        // guarantee, so casting its pointer to `*const f32` would be UB.
        let weights = (0..len)
            .map(|i| {
                let start = (i + 1) * 4;
                let bytes = slice_u8[start..start + 4].try_into().expect("4-byte chunk");
                f32::from_le_bytes(bytes)
            })
            .collect();
        Ok(weights)
    }

    /// Updates morph weights without writing to the GPU.
    pub fn update_morph_weights_with(
        &mut self,
        key: Key,
        f: impl FnOnce(&mut [f32]),
    ) -> Result<()> {
        let len = self.get_info(key).map(|info| info.targets_len())?;

        self.weights.update_with_unchecked(key, |_, slice_u8| {
            // Layout is [count: f32, weight_0, .. weight_{len-1}]. Reinterpret the
            // bytes as `f32` in place — no copy, no alloc — on the per-frame morph
            // path. The `u8 -> f32` cast is sound because the pointer is 4-aligned:
            //  • offset: `DynamicStorageBuffer` is a buddy allocator with
            //    MIN_BLOCK = 256, so every slot offset is a multiple of 256.
            //  • base: the backing `Vec<u8>` is >= 16-byte aligned on wasm32.
            // The debug_assert pins that invariant under test (it fires if the
            // allocator granularity or backing store ever changes); it compiles
            // out of release, so the hot path pays nothing for it.
            debug_assert!(
                slice_u8.as_ptr().cast::<f32>().is_aligned(),
                "morph weights buffer must be f32-aligned"
            );
            let weights_f32 = unsafe {
                std::slice::from_raw_parts_mut(slice_u8.as_mut_ptr() as *mut f32, len + 1)
            };

            // The first value is the count word; expose the `len` weights after it.
            f(&mut weights_f32[1..])
        });

        self.weights_dirty = true;

        Ok(())
    }

    // This *does* write to the gpu, should be called only once per frame
    // just write the entire buffer in one fell swoop
    fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
        bind_group_create_weight_kind: BindGroupCreate,
        bind_group_create_value_kind: BindGroupCreate,
    ) -> Result<()> {
        if self.weights_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Morph Weights GPU write").entered())
            } else {
                None
            };

            let mut resized = false;
            if let Some(new_size) = self.weights.take_gpu_needs_resize() {
                self.gpu_buffer_weights = gpu.create_buffer(
                    &BufferDescriptor::new(Some("Morph Weights"), new_size, *BUFFER_USAGE_WEIGHTS)
                        .into(),
                )?;

                bind_groups.mark_create(bind_group_create_weight_kind);
                resized = true;
            }
            if resized {
                self.weights.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.gpu_buffer_weights,
                    None,
                    self.weights.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let ranges = self.weights.take_dirty_ranges();
                self.weights_uploader.write_dirty_ranges(
                    gpu,
                    &self.gpu_buffer_weights,
                    self.weights.raw_slice().len(),
                    self.weights.raw_slice(),
                    &ranges,
                )?;
            }

            self.weights_dirty = false;
        }
        if self.values_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Morph Values GPU write").entered())
            } else {
                None
            };

            let mut resized = false;
            if let Some(new_size) = self.values.take_gpu_needs_resize() {
                self.gpu_buffer_values = gpu.create_buffer(
                    &BufferDescriptor::new(Some("Morph Values"), new_size, *BUFFER_USAGE_VALUES)
                        .into(),
                )?;

                bind_groups.mark_create(bind_group_create_value_kind);
                resized = true;
            }
            if resized {
                self.values.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.gpu_buffer_values,
                    None,
                    self.values.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let ranges = self.values.take_dirty_ranges();
                self.values_uploader.write_dirty_ranges(
                    gpu,
                    &self.gpu_buffer_values,
                    self.values.raw_slice().len(),
                    self.values.raw_slice(),
                    &ranges,
                )?;
            }

            self.values_dirty = false;
        }

        Ok(())
    }
}

/// Morph target data and GPU buffers.
pub struct MorphData<Key: slotmap::Key, Info> {
    weights: DynamicStorageBuffer<Key>,
    values: DynamicStorageBuffer<Key>,
    weights_dirty: bool,
    values_dirty: bool,
    infos: SlotMap<Key, Info>,
    pub(crate) gpu_buffer_weights: web_sys::GpuBuffer,
    pub(crate) gpu_buffer_values: web_sys::GpuBuffer,
    weights_uploader: crate::buffer::mapped_uploader::MappedUploader,
    values_uploader: crate::buffer::mapped_uploader::MappedUploader,
}

new_key_type! {
    /// Opaque key for geometry morph targets.
    pub struct GeometryMorphKey;
}

new_key_type! {
    /// Opaque key for material morph targets.
    pub struct MaterialMorphKey;
}
