//! Geometry mesh metadata packing.

use std::sync::LazyLock;

use awsm_renderer_core::buffers::BufferUsage;
use slotmap::Key;

use crate::{
    buffer::dynamic_uniform::DynamicUniformBuffer,
    materials::{MaterialKey, Materials},
    meshes::{
        morphs::{GeometryMorphKey, Morphs},
        skins::{SkinKey, Skins},
        AwsmMeshError, MeshKey,
    },
    transforms::{TransformKey, Transforms},
};

/// Byte size for geometry mesh meta struct.
/// 10 u32s + 1 u32 (instance_attr_base) + 1 u32 (billboard_mode) = 48 bytes;
/// the storage buffer rounds each entry up to
/// `GEOMETRY_MESH_META_BYTE_ALIGNMENT`.
pub const GEOMETRY_MESH_META_BYTE_SIZE: usize = 48;
/// Byte alignment for geometry mesh meta buffer entries.
pub const GEOMETRY_MESH_META_BYTE_ALIGNMENT: usize = 256;

// Plan §16.7/§16.8: the geometry-meta GPU buffer is bound at
// `@group(2) @binding(0)` of the geometry pass — as a
// uniform-with-dynamic-offset for instanced meshes (legacy) AND as
// a read-only storage array for non-instanced meshes (new). The
// same physical buffer backs both bindings, so its `usage` flags
// have to satisfy both: `Uniform | Storage | CopyDst`.
pub static GEOMETRY_BUFFER_USAGE: LazyLock<BufferUsage> = LazyLock::new(|| {
    BufferUsage::new()
        .with_copy_dst()
        .with_uniform()
        .with_storage()
});

/// Geometry meta fields used by shaders.
/// See `meta.wgsl` for the corresponding struct.
pub struct GeometryMeshMeta<'a> {
    pub mesh_key: MeshKey,
    pub transform_key: TransformKey,
    pub material_key: MaterialKey,
    pub geometry_morph_key: Option<GeometryMorphKey>,
    pub skin_key: Option<SkinKey>,
    pub materials: &'a Materials,
    pub transforms: &'a Transforms,
    pub morphs: &'a Morphs,
    pub skins: &'a Skins,
    pub material_meta_buffers: &'a DynamicUniformBuffer<MeshKey>,
    /// Per-mesh base offset into the global instance-attribute storage
    /// buffer. The vertex shader adds `@builtin(instance_index)` to this to
    /// derive the per-fragment instance_id that's packed into
    /// barycentric_tex's BA channels and looked up by the shading compute
    /// pass. `u32::MAX` sentinel means "this mesh has no per-instance
    /// attributes" — the shading pass treats that as an identity tint.
    pub instance_attr_base: u32,
    /// Camera-facing rotation override. See `BillboardMode` on `Mesh` for the
    /// authored value. Encoded as `u32` (None=0 / YAxis=1 / Full=2).
    pub billboard_mode: u32,
}

impl<'a> GeometryMeshMeta<'a> {
    /// Packs geometry meta into bytes.
    pub fn to_bytes(
        self,
    ) -> std::result::Result<[u8; GEOMETRY_MESH_META_BYTE_SIZE], AwsmMeshError> {
        let Self {
            mesh_key,
            transform_key,
            material_key: _,
            geometry_morph_key,
            skin_key,
            materials: _,
            transforms,
            morphs,
            skins,
            material_meta_buffers,
            instance_attr_base,
            billboard_mode,
        } = self;

        let mut result = [0u8; GEOMETRY_MESH_META_BYTE_SIZE];
        let mut offset = 0;

        let mut push_u32 = |value: u32| {
            result[offset..offset + 4].copy_from_slice(&value.to_le_bytes());

            offset += 4;
        };

        let mesh_key_u64 = mesh_key.data().as_ffi();
        let (mesh_key_u32_high, mesh_key_u32_low) = (
            (mesh_key_u64 >> 32) as u32,
            (mesh_key_u64 & 0xFFFFFFFF) as u32,
        );

        // Mesh Key (8 bytes)
        push_u32(mesh_key_u32_high);
        push_u32(mesh_key_u32_low);

        // Morph (12 bytes)
        if let Some(morph_key) = geometry_morph_key {
            let info = morphs.geometry.get_info(morph_key)?;
            push_u32(info.targets_len as u32);
            push_u32(morphs.geometry.weights_buffer_offset(morph_key)? as u32);
            push_u32(morphs.geometry.values_buffer_offset(morph_key)? as u32);
        } else {
            push_u32(0);
            push_u32(0);
            push_u32(0);
        }

        // Skin (12 bytes)
        if let Some(skin_key) = skin_key {
            push_u32(skins.sets_len(skin_key)? as u32);
            push_u32(skins.joint_matrices_offset(skin_key)? as u32);
            push_u32(skins.joint_index_weights_offset(skin_key)? as u32);
        } else {
            push_u32(0);
            push_u32(0);
            push_u32(0);
        }

        // Transform (4 bytes)
        push_u32(transforms.buffer_offset(transform_key)? as u32);

        // Material Meta (4 bytes)
        push_u32(
            material_meta_buffers
                .offset(mesh_key)
                .ok_or(AwsmMeshError::MetaNotFound(mesh_key))? as u32,
        );

        // Per-instance attribute base offset (4 bytes; u32::MAX = no attrs)
        push_u32(instance_attr_base);

        // Billboard mode (4 bytes; 0 = None, 1 = YAxis, 2 = Full).
        push_u32(billboard_mode);

        Ok(result)
    }
}
