//! Mesh metadata buffers.

pub mod geometry_meta;
pub mod material_meta;

use awsm_renderer_core::{buffers::BufferDescriptor, renderer::AwsmRendererWebGpu};

use crate::{
    bind_groups::{BindGroupCreate, BindGroups},
    buffer::dynamic_uniform::DynamicUniformBuffer,
    buffer::helpers::write_buffer_with_dirty_ranges,
    debug::AwsmRendererLogging,
    materials::Materials,
    meshes::{
        buffer_info::MeshBufferInfo,
        error::{AwsmMeshError, Result},
        meta::{
            geometry_meta::{
                GeometryMeshMeta, GEOMETRY_BUFFER_USAGE, GEOMETRY_MESH_META_BYTE_ALIGNMENT,
                GEOMETRY_MESH_META_BYTE_SIZE,
            },
            material_meta::{
                MaterialMeshMeta, MATERIAL_BUFFER_USAGE, MATERIAL_MESH_META_BYTE_ALIGNMENT,
                MATERIAL_MESH_META_BYTE_SIZE,
            },
        },
        morphs::Morphs,
        morphs::{GeometryMorphKey, MaterialMorphKey},
        skins::SkinKey,
        skins::Skins,
        Mesh, MeshKey,
    },
    transforms::Transforms,
};

// Reduced from 1024 to stay under 128MB default storage buffer limit.
// Initial visibility buffer size = 512 * 3 * 1000 * 52 = ~76MB
// This is conservative; buffer will grow dynamically as needed.
/// Initial capacity for mesh meta buffers.
pub const MESH_META_INITIAL_CAPACITY: usize = 512;

/// Mesh metadata buffers for geometry and materials.
pub struct MeshMeta {
    // meta data buffers
    geometry_buffers: DynamicUniformBuffer<MeshKey>,
    geometry_gpu_buffer: web_sys::GpuBuffer,
    geometry_dirty: bool,
    // meta data buffers
    material_buffers: DynamicUniformBuffer<MeshKey>,
    material_gpu_buffer: web_sys::GpuBuffer,
    material_dirty: bool,
}

impl MeshMeta {
    /// Creates mesh meta buffers.
    pub fn new(gpu: &AwsmRendererWebGpu) -> Result<Self> {
        Ok(Self {
            geometry_buffers: DynamicUniformBuffer::new(
                MESH_META_INITIAL_CAPACITY,
                GEOMETRY_MESH_META_BYTE_SIZE,
                Some(GEOMETRY_MESH_META_BYTE_ALIGNMENT),
                Some("GeometryMeshMetaData".to_string()),
            ),
            geometry_gpu_buffer: gpu.create_buffer(&<web_sys::GpuBufferDescriptor>::from(
                BufferDescriptor::new(
                    Some("GeometryMeshMetaData"),
                    MESH_META_INITIAL_CAPACITY * GEOMETRY_MESH_META_BYTE_ALIGNMENT,
                    *GEOMETRY_BUFFER_USAGE,
                ),
            ))?,
            geometry_dirty: true,
            material_buffers: DynamicUniformBuffer::new(
                MESH_META_INITIAL_CAPACITY,
                MATERIAL_MESH_META_BYTE_SIZE,
                Some(MATERIAL_MESH_META_BYTE_ALIGNMENT),
                Some("MaterialMeshMetaData".to_string()),
            ),
            material_gpu_buffer: gpu.create_buffer(&<web_sys::GpuBufferDescriptor>::from(
                BufferDescriptor::new(
                    Some("MaterialMeshMetaData"),
                    MESH_META_INITIAL_CAPACITY * MATERIAL_MESH_META_BYTE_ALIGNMENT,
                    *MATERIAL_BUFFER_USAGE,
                ),
            ))?,
            material_dirty: true,
        })
    }
    /// Writes mesh metadata into GPU-bound buffers.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        mesh_key: MeshKey,
        mesh: &Mesh,
        buffer_info: &MeshBufferInfo,
        visibility_geometry_data_offset: Option<usize>,
        custom_attribute_indices_offset: usize,
        custom_attribute_data_offset: usize,
        geometry_morph_key: Option<GeometryMorphKey>,
        material_morph_key: Option<MaterialMorphKey>,
        skin_key: Option<SkinKey>,
        materials: &Materials,
        transforms: &Transforms,
        morphs: &Morphs,
        skins: &Skins,
    ) -> Result<()> {
        let instance_attr_base = mesh.instance_attr_base;
        let transform_key = mesh.transform_key;
        let material_key = mesh.material_key;
        let transform_offset = transforms.buffer_offset(transform_key)?;
        let normal_matrix_offset = transforms.normals_buffer_offset(transform_key)?;

        let meta_data = MaterialMeshMeta {
            mesh_key,
            material_morph_key,
            material_key,
            buffer_info,
            custom_attribute_indices_offset,
            custom_attribute_data_offset,
            visibility_geometry_data_offset,
            transform_offset,
            normal_matrix_offset,
            materials,
            morphs,
            mesh,
        }
        .to_bytes()?;
        self.material_buffers.update(mesh_key, &meta_data);
        self.material_dirty = true;

        let meta_data = GeometryMeshMeta {
            mesh_key,
            material_key,
            transform_key,
            geometry_morph_key,
            skin_key,
            materials,
            transforms,
            morphs,
            skins,
            material_meta_buffers: &self.material_buffers,
            instance_attr_base,
        }
        .to_bytes()?;

        self.geometry_buffers.update(mesh_key, &meta_data);
        self.geometry_dirty = true;

        Ok(())
    }

    /// Returns the GPU buffer for geometry metadata.
    pub fn geometry_gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.geometry_gpu_buffer
    }
    /// Returns the geometry metadata buffer offset for a mesh.
    pub fn geometry_buffer_offset(&self, key: MeshKey) -> Result<usize> {
        self.geometry_buffers
            .offset(key)
            .ok_or(AwsmMeshError::MetaNotFound(key))
    }

    /// Returns the GPU buffer for material metadata.
    pub fn material_gpu_buffer(&self) -> &web_sys::GpuBuffer {
        &self.material_gpu_buffer
    }
    /// Returns the material metadata buffer offset for a mesh.
    pub fn material_buffer_offset(&self, key: MeshKey) -> Result<usize> {
        self.material_buffers
            .offset(key)
            .ok_or(AwsmMeshError::MetaNotFound(key))
    }

    /// Removes mesh metadata entries.
    pub fn remove(&mut self, mesh_key: MeshKey) {
        if self.geometry_buffers.remove(mesh_key) {
            self.geometry_dirty = true;
        }

        if self.material_buffers.remove(mesh_key) {
            self.material_dirty = true;
        }
    }

    /// Writes dirty metadata buffers to the GPU.
    pub fn write_gpu(
        &mut self,
        _logging: &AwsmRendererLogging,
        gpu: &AwsmRendererWebGpu,
        bind_groups: &mut BindGroups,
    ) -> Result<()> {
        if self.geometry_dirty {
            let mut resized = false;
            if let Some(new_size) = self.geometry_buffers.take_gpu_needs_resize() {
                self.geometry_gpu_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(
                        Some("GeometryMeshMetaData"),
                        new_size,
                        *GEOMETRY_BUFFER_USAGE,
                    )
                    .into(),
                )?;
                bind_groups.mark_create(BindGroupCreate::GeometryMeshMetaResize);
                resized = true;
            }

            if resized {
                self.geometry_buffers.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.geometry_gpu_buffer,
                    None,
                    self.geometry_buffers.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let ranges = self.geometry_buffers.take_dirty_ranges();
                write_buffer_with_dirty_ranges(
                    gpu,
                    &self.geometry_gpu_buffer,
                    self.geometry_buffers.raw_slice(),
                    ranges,
                )?;
            }

            self.geometry_dirty = false;
        }

        if self.material_dirty {
            let mut resized = false;
            if let Some(new_size) = self.material_buffers.take_gpu_needs_resize() {
                self.material_gpu_buffer = gpu.create_buffer(
                    &BufferDescriptor::new(
                        Some("MaterialMeshMetaData"),
                        new_size,
                        *MATERIAL_BUFFER_USAGE,
                    )
                    .into(),
                )?;
                bind_groups.mark_create(BindGroupCreate::MaterialMeshMetaResize);
                resized = true;
            }

            if resized {
                self.material_buffers.clear_dirty_ranges();
                gpu.write_buffer(
                    &self.material_gpu_buffer,
                    None,
                    self.material_buffers.raw_slice(),
                    None,
                    None,
                )?;
            } else {
                let ranges = self.material_buffers.take_dirty_ranges();
                write_buffer_with_dirty_ranges(
                    gpu,
                    &self.material_gpu_buffer,
                    self.material_buffers.raw_slice(),
                    ranges,
                )?;
            }

            self.material_dirty = false;
        }

        Ok(())
    }
}

impl Drop for MeshMeta {
    fn drop(&mut self) {
        self.geometry_gpu_buffer.destroy();
        self.material_gpu_buffer.destroy();
    }
}
