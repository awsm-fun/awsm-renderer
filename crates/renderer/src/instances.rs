//! GPU instancing data and buffers.

use awsm_renderer_core::{
    buffers::{BufferDescriptor, BufferUsage},
    error::AwsmCoreError,
    renderer::AwsmRendererWebGpu,
};
use glam::Mat4;
use slotmap::SecondaryMap;
use std::collections::HashSet;
use thiserror::Error;

use crate::{
    bind_groups::AwsmBindGroupError,
    buffer::dynamic_storage::DynamicStorageBuffer,
    buffer::helpers::write_buffer_with_dirty_ranges,
    transforms::{Transform, TransformKey, Transforms},
    AwsmRendererLogging,
};

/// Instance transform storage and GPU buffers.
pub struct Instances {
    transform_buffer: DynamicStorageBuffer<TransformKey>,
    transform_count: SecondaryMap<TransformKey, usize>,
    cpu_transforms: SecondaryMap<TransformKey, Vec<Transform>>,
    gpu_transform_buffer: web_sys::GpuBuffer,
    transform_gpu_dirty: bool,
    transform_dirty: HashSet<TransformKey>,
}

impl Instances {
    /// Initial byte size for instance transforms.
    pub const TRANSFORM_INITIAL_SIZE: usize = Transforms::BYTE_SIZE * 32; // 32 elements is a good starting point

    /// Creates instance buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let transform_buffer = DynamicStorageBuffer::new(
            Self::TRANSFORM_INITIAL_SIZE,
            Some("Instance Transforms".to_string()),
        );

        Ok(Self {
            transform_buffer,
            gpu_transform_buffer: gpu_create_vertex_buffer(gpu, Self::TRANSFORM_INITIAL_SIZE)?,
            transform_count: SecondaryMap::new(),
            cpu_transforms: SecondaryMap::new(),
            transform_gpu_dirty: false,
            transform_dirty: HashSet::new(),
        })
    }

    /// Inserts instance transforms for a key.
    pub fn transform_insert(&mut self, key: TransformKey, transforms: &[Transform]) -> Result<()> {
        // Do the fallible GPU buffer update first so the CPU-side maps are not
        // left with a partially-inserted entry on failure.
        let bytes = Self::transforms_to_bytes(transforms);
        self.transform_buffer.update(key, &bytes).map_err(|e| {
            AwsmInstanceError::BufferCapacityOverflow(format!("instance transforms: {e}"))
        })?;
        self.cpu_transforms.insert(key, transforms.to_vec());
        self.transform_count.insert(key, transforms.len());
        self.transform_gpu_dirty = true;
        self.transform_dirty.insert(key);
        Ok(())
    }

    /// Updates a single instance transform.
    pub fn transform_update(&mut self, key: TransformKey, index: usize, transform: &Transform) {
        if let Some(list) = self.cpu_transforms.get_mut(key) {
            list[index] = transform.clone();
        }
        self.transform_buffer
            .update_with_unchecked(key, |_, bytes| {
                let offset = index * Transforms::BYTE_SIZE;
                let values = transform.to_matrix().to_cols_array();
                let values_u8 = unsafe {
                    std::slice::from_raw_parts(values.as_ptr() as *const u8, Transforms::BYTE_SIZE)
                };

                let slice = &mut bytes[offset..offset + Transforms::BYTE_SIZE];
                slice.copy_from_slice(values_u8);
            });

        self.transform_gpu_dirty = true;
        self.transform_dirty.insert(key);
    }

    /// Appends instance transforms and returns the start index.
    pub fn transform_extend(
        &mut self,
        key: TransformKey,
        transforms: &[Transform],
    ) -> Result<usize> {
        if transforms.is_empty() {
            return Ok(self.transform_instance_count(key).unwrap_or(0));
        }

        let allocated_bytes = self.transform_buffer.allocated_size(key);
        let (start_index, len, can_append) = {
            let list = self
                .cpu_transforms
                .get_mut(key)
                .ok_or(AwsmInstanceError::TransformNotFound(key))?;
            let start_index = list.len();
            list.extend_from_slice(transforms);
            let len = list.len();
            let required_bytes = len * Transforms::BYTE_SIZE;
            let can_append = allocated_bytes
                .map(|allocated| required_bytes <= allocated)
                .unwrap_or(false);

            (start_index, len, can_append)
        };

        if can_append {
            let bytes = Self::transforms_to_bytes(transforms);
            let offset = start_index * Transforms::BYTE_SIZE;
            self.transform_buffer
                .update_with_unchecked(key, |_, buffer| {
                    let end = offset + bytes.len();
                    buffer[offset..end].copy_from_slice(&bytes);
                });
        } else {
            let full_list = self
                .cpu_transforms
                .get(key)
                .ok_or(AwsmInstanceError::TransformNotFound(key))?;
            let full_bytes = Self::transforms_to_bytes(full_list);
            self.transform_buffer
                .update(key, &full_bytes)
                .map_err(|e| {
                    AwsmInstanceError::BufferCapacityOverflow(format!("instance transforms: {e}"))
                })?;
        }
        self.transform_count.insert(key, len);
        self.transform_gpu_dirty = true;
        self.transform_dirty.insert(key);

        Ok(start_index)
    }

    /// Returns the GPU buffer offset for instance transforms.
    pub fn transform_buffer_offset(&self, key: TransformKey) -> Result<usize> {
        self.transform_buffer
            .offset(key)
            .ok_or(AwsmInstanceError::TransformNotFound(key))
    }

    /// Returns the GPU buffer storing instance transforms.
    pub fn gpu_transform_buffer(&self) -> &web_sys::GpuBuffer {
        &self.gpu_transform_buffer
    }

    /// Returns the instance count for a key.
    pub fn transform_instance_count(&self, key: TransformKey) -> Option<usize> {
        self.transform_count.get(key).copied()
    }

    /// Returns the list of transforms for a key.
    pub fn transform_list(&self, key: TransformKey) -> Option<&[Transform]> {
        self.cpu_transforms.get(key).map(|list| list.as_slice())
    }

    /// Returns a single transform by index.
    pub fn get_transform(&self, key: TransformKey, index: usize) -> Option<Transform> {
        if let Some(list) = self.cpu_transforms.get(key) {
            return list.get(index).cloned();
        }

        self.transform_buffer.get(key).and_then(|bytes| {
            let offset = index * Transforms::BYTE_SIZE;
            let slice = bytes.get(offset..offset + Transforms::BYTE_SIZE)?;
            let values_f32 = unsafe {
                std::slice::from_raw_parts(slice.as_ptr() as *const f32, Transforms::BYTE_SIZE / 4)
            };
            let mat = Mat4::from_cols_slice(values_f32);

            Some(Transform::from(mat))
        })
    }

    /// Returns a copy of all transforms for a key.
    pub fn get_transforms(&self, key: TransformKey) -> Option<Vec<Transform>> {
        if let Some(list) = self.cpu_transforms.get(key) {
            return Some(list.clone());
        }

        let count = self.transform_instance_count(key)?;
        let bytes = self.transform_buffer.get(key)?;
        let mut transforms = Vec::with_capacity(count);
        for index in 0..count {
            let offset = index * Transforms::BYTE_SIZE;
            let slice = bytes.get(offset..offset + Transforms::BYTE_SIZE)?;
            let values_f32 = unsafe {
                std::slice::from_raw_parts(slice.as_ptr() as *const f32, Transforms::BYTE_SIZE / 4)
            };
            let mat = Mat4::from_cols_slice(values_f32);
            transforms.push(Transform::from(mat));
        }

        Some(transforms)
    }

    /// Takes and clears dirty transform keys.
    pub fn take_dirty_transforms(&mut self) -> HashSet<TransformKey> {
        std::mem::take(&mut self.transform_dirty)
    }

    // This *does* write to the gpu, should be called only once per frame
    // just write the entire buffer in one fell swoop
    /// Writes instance transforms to the GPU.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
    ) -> Result<()> {
        if self.transform_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings {
                Some(tracing::span!(tracing::Level::INFO, "Instance Transform GPU write").entered())
            } else {
                None
            };

            let mut resized = false;
            if let Some(new_size) = self.transform_buffer.take_gpu_needs_resize() {
                self.gpu_transform_buffer = gpu_create_vertex_buffer(gpu, new_size)?;
                resized = true;
            }

            if resized {
                self.transform_buffer.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.gpu_transform_buffer,
                    None,
                    self.transform_buffer.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let ranges = self.transform_buffer.take_dirty_ranges();
                write_buffer_with_dirty_ranges(
                    gpu,
                    &self.gpu_transform_buffer,
                    self.transform_buffer.raw_slice(),
                    ranges,
                )?;
            }

            self.transform_gpu_dirty = false;
        }
        Ok(())
    }

    fn transforms_to_bytes(transforms: &[Transform]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(transforms.len() * Transforms::BYTE_SIZE);
        for transform in transforms {
            let values = transform.to_matrix().to_cols_array();
            let values_u8 = unsafe {
                std::slice::from_raw_parts(values.as_ptr() as *const u8, Transforms::BYTE_SIZE)
            };
            bytes.extend_from_slice(values_u8);
        }

        bytes
    }

    /// Ensures capacity for additional instances and returns new capacity.
    pub fn transform_reserve(&mut self, key: TransformKey, additional: usize) -> Result<usize> {
        let count = self
            .transform_instance_count(key)
            .ok_or(AwsmInstanceError::TransformNotFound(key))?;
        let desired_count = count + additional;
        let desired_bytes = desired_count * Transforms::BYTE_SIZE;

        let allocated = self
            .transform_buffer
            .allocated_size(key)
            .ok_or(AwsmInstanceError::TransformNotFound(key))?;

        if desired_bytes <= allocated {
            return Ok(allocated / Transforms::BYTE_SIZE);
        }

        let mut existing_bytes = if let Some(list) = self.cpu_transforms.get(key) {
            Self::transforms_to_bytes(list)
        } else if let Some(bytes) = self.transform_buffer.get(key) {
            bytes.to_vec()
        } else {
            return Err(AwsmInstanceError::TransformNotFound(key));
        };

        existing_bytes.resize(desired_bytes, 0);
        self.transform_buffer
            .update(key, &existing_bytes)
            .map_err(|e| {
                AwsmInstanceError::BufferCapacityOverflow(format!("instance transforms: {e}"))
            })?;
        self.transform_gpu_dirty = true;
        self.transform_dirty.insert(key);

        Ok(desired_count)
    }
}

fn gpu_create_vertex_buffer(gpu: &AwsmRendererWebGpu, size: usize) -> Result<web_sys::GpuBuffer> {
    Ok(gpu.create_buffer(
        &BufferDescriptor::new(
            Some("InstanceTransformVertex"),
            size,
            BufferUsage::new().with_copy_dst().with_vertex(),
        )
        .into(),
    )?)
}

/// Result type for instance operations.
type Result<T> = std::result::Result<T, AwsmInstanceError>;

/// Instance-related errors.
#[derive(Error, Debug)]
pub enum AwsmInstanceError {
    #[error("[instance] {0:?}")]
    Core(#[from] AwsmCoreError),

    #[error("[instance] {0:?}")]
    WriteBuffer(#[from] AwsmBindGroupError),

    #[error("[instance] transform does not exist {0:?}")]
    TransformNotFound(TransformKey),

    #[error("[instance] buffer capacity overflow: {0}")]
    BufferCapacityOverflow(String),
}
