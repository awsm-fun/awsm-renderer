//! Mesh metadata buffers.

pub mod geometry_meta;
pub mod material_meta;

use awsm_renderer_core::{buffers::BufferDescriptor, renderer::AwsmRendererWebGpu};

use crate::{
    bind_groups::{BindGroupCreate, BindGroups},
    buffer::dynamic_uniform::DynamicUniformBuffer,
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
    geometry_uploader: crate::buffer::mapped_uploader::MappedUploader,
    material_uploader: crate::buffer::mapped_uploader::MappedUploader,
    /// Last-frame value of the per-mesh shadow-receiver gate. Lets
    /// `set_shadow_receiver_gate` skip the GPU patch when nothing
    /// changed — without this, a 10k-mesh stress scene would patch
    /// every meta slot every frame and the mapped-buffer ring would
    /// spend an unreasonable chunk of its budget on a u32 that hasn't
    /// changed since N frames ago.
    shadow_receiver_gate_cache: slotmap::SecondaryMap<MeshKey, u32>,
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
            geometry_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "GeometryMeshMetaData",
            ),
            material_uploader: crate::buffer::mapped_uploader::MappedUploader::new(
                "MaterialMeshMetaData",
            ),
            shadow_receiver_gate_cache: slotmap::SecondaryMap::new(),
        })
    }

    /// Mapped-ring upload telemetry for the geometry-meta buffer.
    pub fn geometry_upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.geometry_uploader.stats()
    }

    /// Mapped-ring upload telemetry for the material-meta buffer.
    pub fn material_upload_stats(&self) -> crate::buffer::mapped_staging_ring::UploadStats {
        self.material_uploader.stats()
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
        let billboard_mode = mesh.billboard_mode.as_u32();
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

        // Seed the gate cache with the initial packed value
        // (`MaterialMeshMeta::to_bytes` writes `1u` into the
        // `shadow_receiver_gate` slot — the conservative
        // "assume receiver until proven otherwise" default).
        // Without this seed the very first frame's per-mesh
        // `set_shadow_receiver_gate(mesh, 1)` from
        // `LightMeshBuckets::mark_shadow_receivers` would miss the
        // cache (no entry → `Option::None != Some(1)`) and patch
        // every mesh's 4-byte gate slot. On the 10k-mesh stress
        // scene that turned the first frame after a mass-insert
        // into a 40 KB+ dirty-range upload through the mapped
        // ring — pure waste, since the GPU buffer already
        // contained `1` for every entry. Seeding here drops that
        // back to "patch only the meshes whose gate actually
        // flipped to 0 this frame", which on a typical scene is a
        // tiny fraction.
        self.shadow_receiver_gate_cache.insert(mesh_key, 1);

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
            billboard_mode,
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

    /// In-place patch of the `receive_shadows` u32 inside an
    /// already-registered mesh's material metadata. Avoids the full
    /// re-pack that `insert` would require (which needs Materials /
    /// Transforms / Morphs / buffer_info in scope). The next
    /// `write_gpu` flushes the dirty sub-range to the GPU buffer.
    pub fn set_receive_shadows(&mut self, mesh_key: MeshKey, receive_shadows: bool) -> Result<()> {
        if !self.material_buffers.contains_key(mesh_key) {
            return Err(AwsmMeshError::MetaNotFound(mesh_key));
        }
        let value: u32 = if receive_shadows { 1 } else { 0 };
        self.material_buffers.update_offset(
            mesh_key,
            material_meta::MATERIAL_MESH_META_RECEIVE_SHADOWS_OFFSET,
            &value.to_le_bytes(),
        );
        self.material_dirty = true;
        Ok(())
    }

    /// Patches the `material_offset` u32 (offset
    /// `MATERIAL_MESH_META_MATERIAL_OFFSET_OFFSET`) inside an
    /// already-registered mesh's material metadata. Used by the
    /// cheap-material LOD routing in `Meshes::refresh_cheap_material_routing`
    /// to point the GPU at either the authored material or the cheap
    /// variant based on last-frame coverage — same shader_id + same
    /// transparency classification on both sides means swapping the
    /// offset is enough; no pass-routing or pipeline-key changes.
    pub fn set_material_offset(&mut self, mesh_key: MeshKey, offset: u32) -> bool {
        if !self.material_buffers.contains_key(mesh_key) {
            return false;
        }
        self.material_buffers.update_offset(
            mesh_key,
            material_meta::MATERIAL_MESH_META_MATERIAL_OFFSET_OFFSET,
            &offset.to_le_bytes(),
        );
        self.material_dirty = true;
        true
    }

    /// Patches the skin `joint_index_weights` buffer-offset u32 (offset
    /// `GEOMETRY_MESH_META_SKIN_WEIGHTS_OFFSET_OFFSET`) inside an
    /// already-registered mesh's geometry metadata. Used after a
    /// copy-on-write weight edit ([`Skins::make_weights_owned`]) moves an
    /// instance skin onto its own slot — the record otherwise keeps pointing
    /// at the still-shared source stream.
    pub fn set_skin_weights_offset(&mut self, mesh_key: MeshKey, offset: u32) -> bool {
        if !self.geometry_buffers.contains_key(mesh_key) {
            return false;
        }
        self.geometry_buffers.update_offset(
            mesh_key,
            geometry_meta::GEOMETRY_MESH_META_SKIN_WEIGHTS_OFFSET_OFFSET,
            &offset.to_le_bytes(),
        );
        self.geometry_dirty = true;
        true
    }

    /// Patches the per-frame `shadow_receiver_gate` u32 (offset
    /// `MATERIAL_MESH_META_SHADOW_RECEIVER_GATE_OFFSET`) for an
    /// already-registered mesh. Returns whether the patch actually
    /// happened — `false` when the cached last-frame value matched the
    /// new gate, so the caller (per-frame walk) doesn't double-count
    /// "transitions" in any future telemetry.
    ///
    /// The cache is critical for steady-state perf: on a 10k-mesh stress
    /// scene most meshes' gate values don't change frame-to-frame
    /// (lights stay in the same buckets), so skipping the
    /// `update_offset` write keeps the material-meta dirty-range set
    /// sparse — the mapped-buffer ring then uploads only the actual
    /// transitions instead of the entire 2.56 MB buffer.
    pub fn set_shadow_receiver_gate(&mut self, mesh_key: MeshKey, gate: u32) -> bool {
        if !self.material_buffers.contains_key(mesh_key) {
            return false;
        }
        if self.shadow_receiver_gate_cache.get(mesh_key).copied() == Some(gate) {
            return false;
        }
        self.shadow_receiver_gate_cache.insert(mesh_key, gate);
        self.material_buffers.update_offset(
            mesh_key,
            material_meta::MATERIAL_MESH_META_SHADOW_RECEIVER_GATE_OFFSET,
            &gate.to_le_bytes(),
        );
        self.material_dirty = true;
        true
    }

    /// Removes mesh metadata entries.
    pub fn remove(&mut self, mesh_key: MeshKey) {
        if self.geometry_buffers.remove(mesh_key) {
            self.geometry_dirty = true;
        }

        if self.material_buffers.remove(mesh_key) {
            self.material_dirty = true;
        }
        // Drop the cached shadow-gate value so a recycled MeshKey
        // doesn't inherit a stale "no change since last frame" hit.
        self.shadow_receiver_gate_cache.remove(mesh_key);
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
                self.geometry_uploader.write_dirty_ranges(
                    gpu,
                    &self.geometry_gpu_buffer,
                    self.geometry_buffers.raw_slice().len(),
                    self.geometry_buffers.raw_slice(),
                    &ranges,
                )?;
                self.geometry_buffers.recycle_dirty_ranges(ranges);
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
                self.material_uploader.write_dirty_ranges(
                    gpu,
                    &self.material_gpu_buffer,
                    self.material_buffers.raw_slice().len(),
                    self.material_buffers.raw_slice(),
                    &ranges,
                )?;
                self.material_buffers.recycle_dirty_ranges(ranges);
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
