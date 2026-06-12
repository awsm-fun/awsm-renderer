//! Skinning data and GPU updates.

use std::{collections::HashMap, sync::LazyLock};

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};
use glam::Mat4;
use slotmap::{new_key_type, DenseSlotMap, SecondaryMap};
use thiserror::Error;

use crate::{
    bind_groups::{AwsmBindGroupError, BindGroupCreate, BindGroups},
    buffer::dynamic_storage::DynamicStorageBuffer,
    transforms::TransformKey,
    AwsmRendererLogging,
};

/// Skinning data and GPU buffers.
pub struct Skins {
    skeleton_transforms: DenseSlotMap<SkinKey, Vec<TransformKey>>,
    // may be None, in which case its virtually an identity matrix
    inverse_bind_matrices: SecondaryMap<TransformKey, Mat4>,
    sets_len: SecondaryMap<SkinKey, usize>,
    skin_matrices: DynamicStorageBuffer<SkinKey>,
    joint_index_weights: DynamicStorageBuffer<SkinKey>,
    matrices_gpu_dirty: bool,
    joint_index_weights_gpu_dirty: bool,
    /// Skins inserted since the last `update_transforms` pass. Each gets a
    /// ONE-SHOT full joint-matrix refresh (every joint, not just dirty ones)
    /// on the next pass: `insert` can only seed `inverse_bind` (joint worlds
    /// may not be derived yet mid-import), and an async insert can land AFTER
    /// the frame consumed its joints' dirty flags — leaving every un-animated
    /// joint's matrix as bare IBM forever (mesh renders collapsed/shredded;
    /// the editor's mid-session skinned imports hit exactly this).
    pending_full_refresh: Vec<SkinKey>,
    pub(crate) matrices_gpu_buffer: web_sys::GpuBuffer,
    pub(crate) joint_index_weights_gpu_buffer: web_sys::GpuBuffer,
    matrices_uploader: crate::buffer::mapped_uploader::MappedUploader,
    joint_index_weights_uploader: crate::buffer::mapped_uploader::MappedUploader,
}

static BUFFER_USAGE: LazyLock<BufferUsage> =
    LazyLock::new(|| BufferUsage::new().with_storage().with_copy_dst());
impl Skins {
    /// Initial size for skin matrix storage.
    pub const SKIN_MATRICES_INITIAL_SIZE: usize = 16 * 4 * 32; // 32 elements is a good starting point
    /// Initial size for joint index/weight storage.
    pub const JOINT_INDEX_WEIGHTS_INITIAL_SIZE: usize = 4096 * 2; // 4kB (per pair) is a good starting point

    /// Creates skin buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let matrices_gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Skin Matrices"),
                Self::SKIN_MATRICES_INITIAL_SIZE,
                *BUFFER_USAGE,
            )
            .into(),
        )?;

        let joint_index_weights_gpu_buffer = gpu.create_buffer(
            &BufferDescriptor::new(
                Some("Skin Joint Index and Weights"),
                Self::JOINT_INDEX_WEIGHTS_INITIAL_SIZE,
                *BUFFER_USAGE,
            )
            .into(),
        )?;

        Ok(Self {
            skeleton_transforms: DenseSlotMap::with_key(),
            inverse_bind_matrices: SecondaryMap::new(),
            sets_len: SecondaryMap::new(),
            skin_matrices: DynamicStorageBuffer::new(
                Self::SKIN_MATRICES_INITIAL_SIZE,
                Some("Skin Matrices".to_string()),
            ),
            joint_index_weights: DynamicStorageBuffer::new(
                Self::JOINT_INDEX_WEIGHTS_INITIAL_SIZE,
                Some("Skin Joint Index Weights".to_string()),
            ),
            matrices_gpu_dirty: true,
            joint_index_weights_gpu_dirty: true,
            pending_full_refresh: Vec::new(),
            matrices_gpu_buffer,
            joint_index_weights_gpu_buffer,
            matrices_uploader: crate::buffer::mapped_uploader::MappedUploader::new("Skin Matrices"),
            joint_index_weights_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "Skin Joint Index Weights",
            ),
        })
    }

    /// Mapped-ring upload telemetry for the skin matrices buffer.
    pub fn matrices_upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.matrices_uploader.stats()
    }

    /// Mapped-ring upload telemetry for the joint index/weights buffer.
    pub fn joint_index_weights_upload_stats(
        &self,
    ) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.joint_index_weights_uploader.stats()
    }

    /// Inserts a skin and returns its key.
    pub fn insert(
        &mut self,
        skeleton_joint_transforms: Vec<TransformKey>,
        inverse_bind_matrices: &[Mat4],
        set_len: usize,
        joint_index_weights: &[u8],
    ) -> Result<SkinKey> {
        let len = skeleton_joint_transforms.len();
        let mut initial_fill = Vec::with_capacity(len * 16 * 4);

        for (index, joint) in skeleton_joint_transforms.iter().enumerate() {
            // check if the inverse bind matrix has diverged
            match (
                self.inverse_bind_matrices.get(*joint),
                inverse_bind_matrices.get(index),
            ) {
                (None, None) => { /* eh, they're the same, let it go */ }
                (None, Some(_)) => { /* it's probably just a new one, let it go */ }
                (Some(a), Some(b)) if a == b => { /* eh, they're the same, let it go */ }
                _ => {
                    return Err(AwsmSkinError::JointAlreadyExistsButDifferent {
                        joint_transform: *joint,
                    });
                }
            }

            let joint_matrix = inverse_bind_matrices
                .get(index)
                .cloned()
                .unwrap_or(Mat4::IDENTITY);

            //tracing::info!("{}: {:#?}", index, joint_matrix);

            let bytes = unsafe {
                std::slice::from_raw_parts(joint_matrix.as_ref().as_ptr() as *const u8, 16 * 4)
            };
            initial_fill.extend_from_slice(bytes);

            self.inverse_bind_matrices.insert(*joint, joint_matrix);
        }

        let skin_key = self.skeleton_transforms.insert(skeleton_joint_transforms);

        if let Err(e) = self
            .skin_matrices
            .update(skin_key, &initial_fill)
            .map_err(|e| AwsmSkinError::BufferCapacityOverflow(format!("skin matrices: {e}")))
            .and_then(|_| {
                self.joint_index_weights
                    .update(skin_key, joint_index_weights)
                    .map_err(|e| {
                        AwsmSkinError::BufferCapacityOverflow(format!("joint index weights: {e}"))
                    })
            })
        {
            // Roll back partial state so a failed allocation doesn't leave an orphan skin.
            self.skin_matrices.remove(skin_key);
            self.joint_index_weights.remove(skin_key);
            self.skeleton_transforms.remove(skin_key);
            return Err(e);
        }

        self.sets_len.insert(skin_key, set_len);
        self.pending_full_refresh.push(skin_key);

        self.matrices_gpu_dirty = true;
        self.joint_index_weights_gpu_dirty = true;
        Ok(skin_key)
    }

    /// Returns the number of joints in a skin.
    /// Raw copy of a skin's packed per-vertex joint/weight stream. Layout per
    /// ORIGINAL vertex, per set: 4 × (u32 joint index, f32 weight), little-
    /// endian — 32 bytes per set per vertex (see renderer-gltf buffers/skin.rs,
    /// which packs it, and shared_wgsl/vertex/skin.wgsl, which consumes it).
    pub fn read_joint_index_weights(&self, skin_key: SkinKey) -> Result<Vec<u8>> {
        self.joint_index_weights
            .get(skin_key)
            .map(|s| s.to_vec())
            .ok_or(AwsmSkinError::SkinNotFound(skin_key))
    }

    /// CPU copy of the skin's joint-matrix palette (one `joint_world × IBM`
    /// `Mat4` per joint, in the skin's joint-array order) — exactly the
    /// matrices GPU skinning reads. For CPU-side posed-position math: a rest
    /// vertex's deformed WORLD position is `Σ wᵢ · Mᵢ · rest_p` (skinned
    /// vertices are world-space; no node model matrix applies — see
    /// shared_wgsl/vertex/apply_vertex.wgsl).
    pub fn read_joint_matrices(&self, skin_key: SkinKey) -> Result<Vec<Mat4>> {
        let bytes = self
            .skin_matrices
            .get(skin_key)
            .ok_or(AwsmSkinError::SkinNotFound(skin_key))?;
        Ok(bytes
            .chunks_exact(64)
            .map(|c| {
                let mut f = [0.0f32; 16];
                for (i, ch) in c.chunks_exact(4).enumerate() {
                    f[i] = f32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]);
                }
                Mat4::from_cols_array(&f)
            })
            .collect())
    }

    /// In-place edit of the packed joint/weight stream (same layout as
    /// [`Self::read_joint_index_weights`]). Marks the GPU copy dirty — the next
    /// `write_gpu` uploads it, so live skinned meshes re-deform immediately.
    pub fn update_joint_index_weights_with(
        &mut self,
        skin_key: SkinKey,
        f: impl FnOnce(&mut [u8]),
    ) {
        self.joint_index_weights
            .update_with_unchecked(skin_key, |_, bytes| f(bytes));
        self.joint_index_weights_gpu_dirty = true;
    }

    pub fn sets_len(&self, skin_key: SkinKey) -> Result<usize> {
        self.sets_len
            .get(skin_key)
            .cloned()
            .ok_or(AwsmSkinError::SkinNotFound(skin_key))
    }

    /// Returns the GPU buffer offset for joint matrices.
    pub fn joint_matrices_offset(&self, skin_key: SkinKey) -> Result<usize> {
        self.skin_matrices
            .offset(skin_key)
            .ok_or(AwsmSkinError::SkinNotFound(skin_key))
    }

    /// Returns the GPU buffer offset for joint index/weight data.
    pub fn joint_index_weights_offset(&self, skin_key: SkinKey) -> Result<usize> {
        self.joint_index_weights
            .offset(skin_key)
            .ok_or(AwsmSkinError::SkinNotFound(skin_key))
    }

    /// Iterates every live `SkinKey`. Cheap — used by
    /// `Meshes::update_world`'s skinning-LOD gate to discover the set
    /// of skins that exist this frame.
    pub fn iter_skin_keys(&self) -> impl Iterator<Item = SkinKey> + '_ {
        self.skeleton_transforms.keys()
    }

    /// Updates skin matrices from dirty joint transforms.
    ///
    /// `should_update_skin` is consulted once per `SkinKey` and lets the
    /// caller throttle expensive skin matrix refreshes for distant /
    /// low-coverage characters. Default predicate `|_| true`
    /// preserves the previous behaviour.
    pub fn update_transforms(
        &mut self,
        dirty_skin_joints: HashMap<TransformKey, Mat4>,
        transforms: &crate::transforms::Transforms,
        mut should_update_skin: impl FnMut(SkinKey) -> bool,
    ) {
        // One-shot full seed for freshly inserted skins — every joint, from the
        // CURRENT derived worlds, bypassing the dirty set AND the skip gate
        // (this is a correctness seed, not a per-frame refresh). See the field
        // doc on `pending_full_refresh` for why insert can't do this itself.
        for skin_key in std::mem::take(&mut self.pending_full_refresh) {
            let Some(transform_keys) = self.skeleton_transforms.get(skin_key) else {
                continue;
            };
            for (index, transform_key) in transform_keys.clone().iter().enumerate() {
                let Ok(world_mat) = transforms.get_world(*transform_key).copied() else {
                    continue;
                };
                let world_matrix = match self.inverse_bind_matrices.get(*transform_key).cloned() {
                    Some(ibm) => world_mat * ibm,
                    None => world_mat,
                };
                let bytes = unsafe {
                    std::slice::from_raw_parts(world_matrix.as_ref().as_ptr() as *const u8, 16 * 4)
                };
                self.skin_matrices
                    .update_with_unchecked(skin_key, |_, matrices| {
                        let start = index * 16 * 4;
                        matrices[start..start + (16 * 4)].copy_from_slice(bytes);
                    });
                self.matrices_gpu_dirty = true;
            }
        }
        // Diagnostic counters for the editor's pose-doesn't-deform class of bug:
        // a non-empty dirty set that matches ZERO registered joints means the
        // writer (e.g. the editor's skin bridge) is using different
        // TransformKeys than the skin registered — silently nothing updates.
        let mut matched = 0usize;
        let mut skipped_skins = 0usize;
        // different skins can theoretically share the same joint, so, iterate over them all
        for (skin_key, transform_keys) in self.skeleton_transforms.iter() {
            if !should_update_skin(skin_key) {
                skipped_skins += 1;
                continue;
            }
            for (index, transform_key) in transform_keys.iter().enumerate() {
                if let Some(world_mat) = dirty_skin_joints.get(transform_key) {
                    matched += 1;
                    // could cache this for revisited joints, but, it's not a huge deal - might even be faster to redo the math
                    let world_matrix = match self.inverse_bind_matrices.get(*transform_key).cloned()
                    {
                        Some(inverse_bind_matrix) => *world_mat * inverse_bind_matrix,
                        None => *world_mat,
                    };

                    // just overwrite this one matrix
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            world_matrix.as_ref().as_ptr() as *const u8,
                            16 * 4,
                        )
                    };

                    self.skin_matrices
                        .update_with_unchecked(skin_key, |_, matrices| {
                            let start = index * 16 * 4;
                            matrices[start..start + (16 * 4)].copy_from_slice(bytes);
                        });

                    self.matrices_gpu_dirty = true;
                }
            }

            //tracing::info!("{:#?}", u8_to_f32_vec(&self.skin_matrices.raw_slice()[self.skin_matrices.offset(skin_key).unwrap()..]).chunks(16).take(2).collect::<Vec<_>>());
        }
        if matched > 0 || skipped_skins > 0 {
            tracing::debug!(
                "skins.update_transforms: {} joint matrices updated, {} skins skipped (dirty set: {})",
                matched,
                skipped_skins,
                dirty_skin_joints.len(),
            );
        }
    }

    /// Returns `true` when no skin is registered. Cheap (one DenseSlotMap
    /// length read); used by `write_gpu` to skip its work entirely on
    /// scenes without any skinned meshes.
    pub fn is_empty(&self) -> bool {
        self.skeleton_transforms.is_empty()
    }

    /// Writes skin buffers to the GPU.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        // Scenes without any skins — the common case — pay nothing
        // beyond this length read. Skipping the inner writes avoids the
        // first-frame `writeBuffer` of the empty 2 kB / 8 kB starter
        // buffers (`*_gpu_dirty` defaults to `true` at construction).
        if self.is_empty() {
            return Ok(());
        }
        if self.matrices_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Skin Matrices GPU write").entered())
            } else {
                None
            };

            let mut resized = false;
            if let Some(new_size) = self.skin_matrices.take_gpu_needs_resize() {
                self.matrices_gpu_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(Some("Skins"), new_size, *BUFFER_USAGE).into(),
                )?;

                bind_groups.mark_create(BindGroupCreate::SkinJointMatricesResize);
                resized = true;
            }

            if resized {
                self.skin_matrices.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.matrices_gpu_buffer,
                    None,
                    self.skin_matrices.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let ranges = self.skin_matrices.take_dirty_ranges();
                self.matrices_uploader.write_dirty_ranges(
                    gpu,
                    &self.matrices_gpu_buffer,
                    self.skin_matrices.raw_slice().len(),
                    self.skin_matrices.raw_slice(),
                    &ranges,
                )?;
            }

            self.matrices_gpu_dirty = false;
        }

        if self.joint_index_weights_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(
                    tracing::span!(tracing::Level::INFO, "Skin Joint Index Weights GPU write")
                        .entered(),
                )
            } else {
                None
            };

            let mut resized = false;
            if let Some(new_size) = self.joint_index_weights.take_gpu_needs_resize() {
                self.joint_index_weights_gpu_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(
                        Some("Skin Joint Index and Weights"),
                        new_size,
                        *BUFFER_USAGE,
                    )
                    .into(),
                )?;

                bind_groups.mark_create(BindGroupCreate::SkinJointIndexAndWeightsResize);
                resized = true;
            }

            if resized {
                self.joint_index_weights.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.joint_index_weights_gpu_buffer,
                    None,
                    self.joint_index_weights.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let ranges = self.joint_index_weights.take_dirty_ranges();
                self.joint_index_weights_uploader.write_dirty_ranges(
                    gpu,
                    &self.joint_index_weights_gpu_buffer,
                    self.joint_index_weights.raw_slice().len(),
                    self.joint_index_weights.raw_slice(),
                    &ranges,
                )?;
            }

            self.joint_index_weights_gpu_dirty = false;
        }

        Ok(())
    }

    /// Removes a skin and associated data.
    pub fn remove(&mut self, key: SkinKey, transform: Option<TransformKey>) {
        self.skeleton_transforms.remove(key);
        self.skin_matrices.remove(key);
        self.joint_index_weights.remove(key);
        if let Some(transform) = transform {
            self.inverse_bind_matrices.remove(transform);
        }
        self.matrices_gpu_dirty = true;
        self.joint_index_weights_gpu_dirty = true;
    }
}

new_key_type! {
    /// Opaque key for skins.
    pub struct SkinKey;
}

/// Result type for skin operations.
pub type Result<T> = std::result::Result<T, AwsmSkinError>;

/// Skin-related errors.
#[derive(Error, Debug)]
pub enum AwsmSkinError {
    #[error("[skin] {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[skin] skin not found: {0:?}")]
    SkinNotFound(SkinKey),

    #[error("[skin] joint transform not found: {joint_transform:?}")]
    JointTransformNotFound { joint_transform: TransformKey },

    #[error("[skin] skin joint matrix mismatch, skin: {skin_key:?}, matrix len: {matrix_len:?} joint_len: {joint_len:?}")]
    SkinJointMatrixMismatch {
        skin_key: SkinKey,
        matrix_len: usize,
        joint_len: usize,
    },

    #[error("[skin] joint already exists but is different: {joint_transform:?}")]
    JointAlreadyExistsButDifferent { joint_transform: TransformKey },

    #[error("[skin] {0:?}")]
    BindGroup(#[from] AwsmBindGroupError),

    #[error("[skin] buffer capacity overflow: {0}")]
    BufferCapacityOverflow(String),
}
