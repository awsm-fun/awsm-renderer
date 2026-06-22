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
    bind_groups::{AwsmBindGroupError, BindGroupCreate, BindGroups},
    buffer::dynamic_storage::DynamicStorageBuffer,
    transforms::{Transform, TransformKey},
    AwsmRendererLogging,
};

/// Stride of one entry in the per-instance transform buffer: a single
/// `mat4x4<f32>` world matrix (16 × f32 = 64 bytes). This MUST equal the
/// per-instance vertex-buffer `array_stride` bound by the geometry / shadow
/// / transparent pipelines, which the GPU reads as 4 × `vec4<f32>` rows
/// (`MeshBufferVertexInfo::INSTANCING_BYTE_SIZE`).
///
/// It is deliberately **not** `Transforms::BYTE_SIZE` (112): node transforms
/// additionally carry a normal matrix, but per-instance entries carry only
/// the world matrix. Reusing the 112-byte node stride here wrote each
/// instance 48 bytes too far apart (and read 48 bytes of out-of-bounds
/// garbage per matrix), so the GPU — reading at 64-byte stride — saw every
/// instance after the first as a garbage matrix and exploded the geometry.
const INSTANCE_TRANSFORM_BYTE_SIZE: usize =
    crate::meshes::buffer_info::MeshBufferVertexInfo::INSTANCING_BYTE_SIZE;

/// Per-instance attributes consumed by the shading pass.
///
/// Layout (16 bytes, matches `InstanceAttr` in `shared_wgsl/instance_attrs.wgsl`):
/// - `color_packed` — RGBA8 unorm packed into a `u32` (low byte = R), unpacked
///   via WGSL `unpack4x8unorm`.
/// - `size` — per-instance uniform scale. Stage-3 bakes this into the per-instance
///   transform on the CPU side; the field is retained in the GPU struct for future
///   use by a GPU-compute particle simulator that wants to leave transforms static
///   and rewrite only the attribute buffer per frame.
/// - `alpha` — multiplicative alpha applied on top of the material's base alpha.
/// - `_pad` — keeps the struct 16-byte aligned for WebGPU storage layout.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InstanceAttr {
    pub color_packed: u32,
    pub size: f32,
    pub alpha: f32,
    pub _pad: u32,
}

impl Default for InstanceAttr {
    fn default() -> Self {
        Self {
            color_packed: 0xFFFFFFFF,
            size: 1.0,
            alpha: 1.0,
            _pad: 0,
        }
    }
}

impl InstanceAttr {
    /// Number of bytes per `InstanceAttr` in the GPU storage buffer.
    pub const BYTE_SIZE: usize = 16;

    /// Packs an `[r, g, b, a]` 0..=1 color into the storage format.
    pub fn from_rgba_alpha_size(rgba: [f32; 4], alpha_mul: f32, size: f32) -> Self {
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let r = to_u8(rgba[0]);
        let g = to_u8(rgba[1]);
        let b = to_u8(rgba[2]);
        let a = to_u8(rgba[3]);
        let color_packed =
            u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16) | (u32::from(a) << 24);
        Self {
            color_packed,
            size,
            alpha: alpha_mul,
            _pad: 0,
        }
    }
}

/// Instance transform storage and GPU buffers.
pub struct Instances {
    transform_buffer: DynamicStorageBuffer<TransformKey>,
    transform_count: SecondaryMap<TransformKey, usize>,
    cpu_transforms: SecondaryMap<TransformKey, Vec<Transform>>,
    gpu_transform_buffer: web_sys::GpuBuffer,
    transform_gpu_dirty: bool,
    transform_dirty: HashSet<TransformKey>,
    // Per-instance attribute block parallel to the transform buffer. Keyed by the
    // same `TransformKey` so a mesh's instance attributes live next to its
    // transforms.
    attribute_buffer: DynamicStorageBuffer<TransformKey>,
    attribute_count: SecondaryMap<TransformKey, usize>,
    cpu_attributes: SecondaryMap<TransformKey, Vec<InstanceAttr>>,
    gpu_attribute_buffer: web_sys::GpuBuffer,
    attribute_gpu_dirty: bool,
    transform_uploader: crate::buffer::mapped_uploader::MappedUploader,
    attribute_uploader: crate::buffer::mapped_uploader::MappedUploader,
}

impl Instances {
    /// Initial byte size for instance transforms.
    pub const TRANSFORM_INITIAL_SIZE: usize = INSTANCE_TRANSFORM_BYTE_SIZE * 32; // 32 elements is a good starting point
    /// Initial byte size for instance attributes.
    pub const ATTRIBUTE_INITIAL_SIZE: usize = InstanceAttr::BYTE_SIZE * 32;

    /// Creates instance buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        let transform_buffer = DynamicStorageBuffer::new(
            Self::TRANSFORM_INITIAL_SIZE,
            Some("Instance Transforms".to_string()),
        );
        let attribute_buffer = DynamicStorageBuffer::new(
            Self::ATTRIBUTE_INITIAL_SIZE,
            Some("Instance Attributes".to_string()),
        );

        Ok(Self {
            transform_buffer,
            gpu_transform_buffer: gpu_create_vertex_buffer(gpu, Self::TRANSFORM_INITIAL_SIZE)?,
            transform_count: SecondaryMap::new(),
            cpu_transforms: SecondaryMap::new(),
            transform_gpu_dirty: false,
            transform_dirty: HashSet::new(),
            attribute_buffer,
            gpu_attribute_buffer: gpu_create_storage_buffer(gpu, Self::ATTRIBUTE_INITIAL_SIZE)?,
            attribute_count: SecondaryMap::new(),
            cpu_attributes: SecondaryMap::new(),
            attribute_gpu_dirty: false,
            transform_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "Instance Transforms",
            ),
            attribute_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "Instance Attributes",
            ),
        })
    }

    /// Mapped-ring upload telemetry for the instance transform buffer.
    pub fn transform_upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.transform_uploader.stats()
    }

    /// Mapped-ring upload telemetry for the instance attribute buffer.
    pub fn attribute_upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.attribute_uploader.stats()
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

    /// In-place overwrite of ALL of `key`'s instance transforms when the
    /// COUNT is unchanged — the per-frame particle/instancing refresh path:
    /// bytes are written straight into the existing buffer slot and the CPU
    /// mirror is overwritten in place, so a steady-state tick allocates
    /// NOTHING (vs `transform_insert`'s fresh byte Vec + `to_vec` mirror
    /// every call). Falls back to the insert path when the count differs
    /// (shape changes are rare; an alloc there is fine).
    pub fn transform_write_all(
        &mut self,
        key: TransformKey,
        transforms: &[Transform],
    ) -> Result<()> {
        if self.transform_count.get(key).copied() != Some(transforms.len()) {
            return self.transform_insert(key, transforms);
        }
        self.transform_buffer
            .update_with_unchecked(key, |_, bytes| {
                for (i, transform) in transforms.iter().enumerate() {
                    let values = transform.to_matrix().to_cols_array();
                    let values_u8 = unsafe {
                        std::slice::from_raw_parts(
                            values.as_ptr() as *const u8,
                            INSTANCE_TRANSFORM_BYTE_SIZE,
                        )
                    };
                    let offset = i * INSTANCE_TRANSFORM_BYTE_SIZE;
                    bytes[offset..offset + INSTANCE_TRANSFORM_BYTE_SIZE].copy_from_slice(values_u8);
                }
            });
        if let Some(list) = self.cpu_transforms.get_mut(key) {
            list.clone_from_slice(transforms);
        }
        self.transform_gpu_dirty = true;
        self.transform_dirty.insert(key);
        Ok(())
    }

    /// Overwrite ALL of `key`'s instance transforms straight from a raw
    /// byte slice in GPU layout (`INSTANCE_TRANSFORM_BYTE_SIZE` per
    /// instance = one column-major `Mat4`). This is the shared-memory
    /// sim-state path (`docs/PLAYER-GUIDE.md §9`, M4): a physics
    /// worker writes per-instance world `Mat4`s into a shared
    /// [`SharedArena`](crate::buffer::shared_arena::SharedArena); the render
    /// worker hands that arena's contiguous mirror straight here — no
    /// `Transform` round-trip, no per-frame allocation.
    ///
    /// `bytes.len()` must equal `count * INSTANCE_TRANSFORM_BYTE_SIZE` (the
    /// instance count is topology — owner-managed). The CPU `Transform`
    /// mirror is intentionally NOT refreshed (the sim owns these values);
    /// bounds derived from it are one frame stale, same trade-off as the
    /// node-transform arena path.
    pub fn transform_write_all_bytes(&mut self, key: TransformKey, bytes: &[u8]) -> Result<()> {
        let count = self.transform_count.get(key).copied().unwrap_or(0);
        let expected = count * INSTANCE_TRANSFORM_BYTE_SIZE;
        if bytes.len() != expected {
            return Err(AwsmInstanceError::BufferCapacityOverflow(format!(
                "instance transform bytes: got {}, expected {expected}",
                bytes.len()
            )));
        }
        self.transform_buffer.update_with_unchecked(key, |_, dst| {
            dst[..bytes.len()].copy_from_slice(bytes);
        });
        self.transform_gpu_dirty = true;
        Ok(())
    }

    /// Overwrite ALL of `key`'s per-instance attributes from a raw byte
    /// slice in GPU layout (`InstanceAttr::BYTE_SIZE` per instance). The
    /// attribute counterpart to [`Self::transform_write_all_bytes`] (M4).
    /// `bytes.len()` must equal `count * InstanceAttr::BYTE_SIZE`.
    pub fn attribute_write_all_bytes(&mut self, key: TransformKey, bytes: &[u8]) -> Result<()> {
        let count = self.attribute_count.get(key).copied().unwrap_or(0);
        let expected = count * InstanceAttr::BYTE_SIZE;
        if bytes.len() != expected {
            return Err(AwsmInstanceError::BufferCapacityOverflow(format!(
                "instance attribute bytes: got {}, expected {expected}",
                bytes.len()
            )));
        }
        self.attribute_buffer.update_with_unchecked(key, |_, dst| {
            dst[..bytes.len()].copy_from_slice(bytes);
        });
        self.attribute_gpu_dirty = true;
        Ok(())
    }

    /// Updates a single instance transform.
    pub fn transform_update(&mut self, key: TransformKey, index: usize, transform: &Transform) {
        if let Some(list) = self.cpu_transforms.get_mut(key) {
            list[index] = transform.clone();
        }
        self.transform_buffer
            .update_with_unchecked(key, |_, bytes| {
                let offset = index * INSTANCE_TRANSFORM_BYTE_SIZE;
                let values = transform.to_matrix().to_cols_array();
                let values_u8 = unsafe {
                    std::slice::from_raw_parts(
                        values.as_ptr() as *const u8,
                        INSTANCE_TRANSFORM_BYTE_SIZE,
                    )
                };

                let slice = &mut bytes[offset..offset + INSTANCE_TRANSFORM_BYTE_SIZE];
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
            let required_bytes = len * INSTANCE_TRANSFORM_BYTE_SIZE;
            let can_append = allocated_bytes
                .map(|allocated| required_bytes <= allocated)
                .unwrap_or(false);

            (start_index, len, can_append)
        };

        if can_append {
            let bytes = Self::transforms_to_bytes(transforms);
            let offset = start_index * INSTANCE_TRANSFORM_BYTE_SIZE;
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

    /// Inserts (or replaces) the per-instance attribute slice for a key.
    pub fn attribute_insert(
        &mut self,
        key: TransformKey,
        attributes: &[InstanceAttr],
    ) -> Result<()> {
        let bytes = Self::attributes_to_bytes(attributes);
        self.attribute_buffer.update(key, &bytes).map_err(|e| {
            AwsmInstanceError::BufferCapacityOverflow(format!("instance attributes: {e}"))
        })?;
        self.cpu_attributes.insert(key, attributes.to_vec());
        self.attribute_count.insert(key, attributes.len());
        self.attribute_gpu_dirty = true;
        Ok(())
    }

    /// In-place overwrite of ALL of `key`'s instance attributes when the
    /// COUNT is unchanged — see [`Self::transform_write_all`] (same per-frame
    /// zero-alloc rationale). Falls back to the insert path on count change.
    pub fn attribute_write_all(
        &mut self,
        key: TransformKey,
        attributes: &[InstanceAttr],
    ) -> Result<()> {
        if self.attribute_count.get(key).copied() != Some(attributes.len()) {
            return self.attribute_insert(key, attributes);
        }
        self.attribute_buffer
            .update_with_unchecked(key, |_, bytes| {
                for (i, attr) in attributes.iter().enumerate() {
                    let offset = i * InstanceAttr::BYTE_SIZE;
                    bytes[offset..offset + InstanceAttr::BYTE_SIZE]
                        .copy_from_slice(&Self::attribute_to_bytes(attr));
                }
            });
        if let Some(list) = self.cpu_attributes.get_mut(key) {
            list.copy_from_slice(attributes);
        }
        self.attribute_gpu_dirty = true;
        Ok(())
    }

    /// Updates a single per-instance attribute in-place.
    pub fn attribute_update(&mut self, key: TransformKey, index: usize, attr: &InstanceAttr) {
        if let Some(list) = self.cpu_attributes.get_mut(key) {
            list[index] = *attr;
        }
        self.attribute_buffer
            .update_with_unchecked(key, |_, bytes| {
                let offset = index * InstanceAttr::BYTE_SIZE;
                let attr_bytes = Self::attribute_to_bytes(attr);
                bytes[offset..offset + InstanceAttr::BYTE_SIZE].copy_from_slice(&attr_bytes);
            });
        self.attribute_gpu_dirty = true;
    }

    /// Grow the per-instance attribute slice for `key` by `additional`
    /// default `InstanceAttr` entries (white tint, alpha 1.0, size 1.0).
    /// No-op if attributes haven't been set for this key — callers that
    /// haven't bound attrs don't need a parallel buffer.
    ///
    /// Used by `append_mesh_instances` / `reserve_mesh_instances` to
    /// keep `attribute_count == transform_count` invariant after a
    /// transform append, so the shading pass's
    /// `instance_attrs[base + instance_index]` lookup never reads past
    /// the logical slice.
    pub fn attribute_extend_with_default(
        &mut self,
        key: TransformKey,
        additional: usize,
    ) -> Result<()> {
        if additional == 0 {
            return Ok(());
        }
        if !self.cpu_attributes.contains_key(key) {
            return Ok(());
        }
        let existing = self
            .cpu_attributes
            .get(key)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let new_len = existing.len() + additional;
        let mut next = Vec::with_capacity(new_len);
        next.extend_from_slice(existing);
        for _ in 0..additional {
            next.push(InstanceAttr::default());
        }
        let bytes = Self::attributes_to_bytes(&next);
        self.attribute_buffer.update(key, &bytes).map_err(|e| {
            AwsmInstanceError::BufferCapacityOverflow(format!("instance attributes: {e}"))
        })?;
        self.attribute_count.insert(key, new_len);
        self.cpu_attributes.insert(key, next);
        self.attribute_gpu_dirty = true;
        Ok(())
    }

    /// Removes the per-instance attribute slice for a key.
    pub fn attribute_remove(&mut self, key: TransformKey) {
        self.attribute_buffer.remove(key);
        self.cpu_attributes.remove(key);
        self.attribute_count.remove(key);
        self.attribute_gpu_dirty = true;
    }

    /// Returns the byte offset into the GPU attribute buffer for a key. The
    /// vertex / shading passes divide this by `InstanceAttr::BYTE_SIZE` to get
    /// an instance-index base used to look up per-fragment tints.
    pub fn attribute_buffer_offset(&self, key: TransformKey) -> Option<usize> {
        self.attribute_buffer.offset(key)
    }

    /// Returns the number of per-instance attributes for a key.
    pub fn attribute_instance_count(&self, key: TransformKey) -> Option<usize> {
        self.attribute_count.get(key).copied()
    }

    /// Returns the GPU buffer storing per-instance attributes.
    pub fn gpu_attribute_buffer(&self) -> &web_sys::GpuBuffer {
        &self.gpu_attribute_buffer
    }

    fn attribute_to_bytes(attr: &InstanceAttr) -> [u8; InstanceAttr::BYTE_SIZE] {
        let mut out = [0u8; InstanceAttr::BYTE_SIZE];
        out[0..4].copy_from_slice(&attr.color_packed.to_le_bytes());
        out[4..8].copy_from_slice(&attr.size.to_le_bytes());
        out[8..12].copy_from_slice(&attr.alpha.to_le_bytes());
        out[12..16].copy_from_slice(&attr._pad.to_le_bytes());
        out
    }

    fn attributes_to_bytes(attributes: &[InstanceAttr]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(attributes.len() * InstanceAttr::BYTE_SIZE);
        for attr in attributes {
            bytes.extend_from_slice(&Self::attribute_to_bytes(attr));
        }
        bytes
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
            let offset = index * INSTANCE_TRANSFORM_BYTE_SIZE;
            let slice = bytes.get(offset..offset + INSTANCE_TRANSFORM_BYTE_SIZE)?;
            let values_f32 = unsafe {
                std::slice::from_raw_parts(
                    slice.as_ptr() as *const f32,
                    INSTANCE_TRANSFORM_BYTE_SIZE / 4,
                )
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
            let offset = index * INSTANCE_TRANSFORM_BYTE_SIZE;
            let slice = bytes.get(offset..offset + INSTANCE_TRANSFORM_BYTE_SIZE)?;
            let values_f32 = unsafe {
                std::slice::from_raw_parts(
                    slice.as_ptr() as *const f32,
                    INSTANCE_TRANSFORM_BYTE_SIZE / 4,
                )
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
    /// Writes instance transforms and per-instance attributes to the GPU.
    pub fn write_gpu(
        &mut self,
        logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        if self.transform_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
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
                self.transform_uploader.write_dirty_ranges(
                    gpu,
                    &self.gpu_transform_buffer,
                    self.transform_buffer.raw_slice().len(),
                    self.transform_buffer.raw_slice(),
                    &ranges,
                )?;
            }

            self.transform_gpu_dirty = false;
        }

        if self.attribute_gpu_dirty {
            let _maybe_span_guard = if logging.render_timings.sub_frame() {
                Some(tracing::span!(tracing::Level::INFO, "Instance Attribute GPU write").entered())
            } else {
                None
            };

            let mut resized = false;
            if let Some(new_size) = self.attribute_buffer.take_gpu_needs_resize() {
                self.gpu_attribute_buffer = gpu_create_storage_buffer(gpu, new_size)?;
                bind_groups.mark_create(BindGroupCreate::InstanceAttributesResize);
                resized = true;
            }

            if resized {
                self.attribute_buffer.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.gpu_attribute_buffer,
                    None,
                    self.attribute_buffer.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let ranges = self.attribute_buffer.take_dirty_ranges();
                self.attribute_uploader.write_dirty_ranges(
                    gpu,
                    &self.gpu_attribute_buffer,
                    self.attribute_buffer.raw_slice().len(),
                    self.attribute_buffer.raw_slice(),
                    &ranges,
                )?;
            }

            self.attribute_gpu_dirty = false;
        }
        Ok(())
    }

    fn transforms_to_bytes(transforms: &[Transform]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(transforms.len() * INSTANCE_TRANSFORM_BYTE_SIZE);
        for transform in transforms {
            let values = transform.to_matrix().to_cols_array();
            let values_u8 = unsafe {
                std::slice::from_raw_parts(
                    values.as_ptr() as *const u8,
                    INSTANCE_TRANSFORM_BYTE_SIZE,
                )
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
        let desired_bytes = desired_count * INSTANCE_TRANSFORM_BYTE_SIZE;

        let allocated = self
            .transform_buffer
            .allocated_size(key)
            .ok_or(AwsmInstanceError::TransformNotFound(key))?;

        if desired_bytes <= allocated {
            return Ok(allocated / INSTANCE_TRANSFORM_BYTE_SIZE);
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

fn gpu_create_storage_buffer(gpu: &AwsmRendererWebGpu, size: usize) -> Result<web_sys::GpuBuffer> {
    Ok(gpu.create_buffer(
        &BufferDescriptor::new(
            Some("InstanceAttributes"),
            size,
            BufferUsage::new().with_copy_dst().with_storage(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, Vec3};

    /// Regression for the per-instance transform stride bug fixed in
    /// `5d34e71` (PR #109). The GPU per-instance vertex buffer is bound at a
    /// 64-byte `mat4x4<f32>` stride; before the fix the offsets used the
    /// 112-byte node-transform stride (matrix + normal matrix), so every
    /// instance *after the first* was packed 48 bytes too far apart and the
    /// GPU read garbage for it — garbling the geometry of any mesh with >1
    /// instance.
    ///
    /// Exercise ≥2 instances (the >1 case is exactly what was broken) and
    /// confirm each one lands on the 64-byte boundary and round-trips, so the
    /// stride can never silently regress to the 112-byte node stride again.
    #[test]
    fn per_instance_transforms_packed_at_64_byte_stride() {
        // The stride constant itself: equal to the GPU-bound vertex stride,
        // and explicitly NOT the 112-byte node stride.
        assert_eq!(INSTANCE_TRANSFORM_BYTE_SIZE, 64);
        assert_eq!(
            INSTANCE_TRANSFORM_BYTE_SIZE,
            crate::meshes::buffer_info::MeshBufferVertexInfo::INSTANCING_BYTE_SIZE,
        );
        assert_ne!(
            INSTANCE_TRANSFORM_BYTE_SIZE,
            crate::transforms::Transforms::BYTE_SIZE,
            "per-instance stride must not be the 112-byte node-transform stride",
        );

        // Three distinct instances, each with a different translation/scale so
        // a wrong stride would read a neighbour's bytes and fail the compare.
        let transforms = vec![
            Transform {
                translation: Vec3::new(1.0, 2.0, 3.0),
                rotation: Quat::IDENTITY,
                scale: Vec3::splat(1.0),
            },
            Transform {
                translation: Vec3::new(-4.0, 5.0, -6.0),
                rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_3),
                scale: Vec3::new(2.0, 0.5, 1.5),
            },
            Transform {
                translation: Vec3::new(7.0, -8.0, 9.0),
                rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_4),
                scale: Vec3::splat(3.0),
            },
        ];

        let bytes = Instances::transforms_to_bytes(&transforms);
        assert_eq!(
            bytes.len(),
            transforms.len() * INSTANCE_TRANSFORM_BYTE_SIZE,
            "packed buffer must be exactly one 64-byte entry per instance",
        );

        // Read each instance back at the 64-byte stride (the same offset math
        // the GPU vertex fetch uses) and confirm it round-trips.
        for (index, expected) in transforms.iter().enumerate() {
            let offset = index * INSTANCE_TRANSFORM_BYTE_SIZE;
            let slice = &bytes[offset..offset + INSTANCE_TRANSFORM_BYTE_SIZE];
            let values_f32 = unsafe {
                std::slice::from_raw_parts(
                    slice.as_ptr() as *const f32,
                    INSTANCE_TRANSFORM_BYTE_SIZE / 4,
                )
            };
            let got = Mat4::from_cols_slice(values_f32).to_cols_array();
            let want = expected.to_matrix().to_cols_array();
            for (g, w) in got.iter().zip(want.iter()) {
                assert!(
                    (g - w).abs() < 1e-6,
                    "instance {index} matrix mismatch at the 64-byte stride: got {got:?}, want {want:?}",
                );
            }
        }
    }
}
