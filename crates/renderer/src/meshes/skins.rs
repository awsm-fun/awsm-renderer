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
    buffer::helpers::write_buffer_with_dirty_ranges,
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
    pub(crate) matrices_gpu_buffer: web_sys::GpuBuffer,
    pub(crate) joint_index_weights_gpu_buffer: web_sys::GpuBuffer,
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
            matrices_gpu_buffer,
            joint_index_weights_gpu_buffer,
        })
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

        self.matrices_gpu_dirty = true;
        self.joint_index_weights_gpu_dirty = true;
        Ok(skin_key)
    }

    /// Returns the number of joints in a skin.
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

    /// Updates skin matrices from dirty joint transforms.
    pub fn update_transforms(&mut self, dirty_skin_joints: HashMap<TransformKey, Mat4>) {
        // different skins can theoretically share the same joint, so, iterate over them all
        for (skin_key, transform_keys) in self.skeleton_transforms.iter() {
            for (index, transform_key) in transform_keys.iter().enumerate() {
                if let Some(world_mat) = dirty_skin_joints.get(transform_key) {
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
    }

    /// Writes skin buffers to the GPU.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        if self.matrices_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings {
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
                write_buffer_with_dirty_ranges(
                    gpu,
                    &self.matrices_gpu_buffer,
                    self.skin_matrices.raw_slice(),
                    ranges,
                )?;
            }

            self.matrices_gpu_dirty = false;
        }

        if self.joint_index_weights_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings {
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
                write_buffer_with_dirty_ranges(
                    gpu,
                    &self.joint_index_weights_gpu_buffer,
                    self.joint_index_weights.raw_slice(),
                    ranges,
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
