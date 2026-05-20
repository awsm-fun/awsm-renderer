//! Material mesh metadata packing.

use std::sync::LazyLock;

use awsm_renderer_core::buffers::BufferUsage;
use slotmap::Key;

use crate::{
    materials::{MaterialKey, Materials},
    meshes::{
        buffer_info::MeshBufferInfo,
        buffer_info::{MeshBufferCustomVertexAttributeInfo, MeshBufferVertexAttributeInfo},
        morphs::{MaterialMorphKey, Morphs},
        AwsmMeshError, Mesh, MeshKey,
    },
};

/// Bitmask for normal morphing.
pub const MATERIAL_MESH_META_MORPH_MATERIAL_BITMASK_NORMAL: u32 = 1;
/// Bitmask for tangent morphing.
pub const MATERIAL_MESH_META_MORPH_MATERIAL_BITMASK_TANGENT: u32 = 1 << 1;
/// Byte size for material mesh meta struct.
pub const MATERIAL_MESH_META_BYTE_SIZE: usize = 72;
/// Byte alignment for material mesh meta entries.
pub const MATERIAL_MESH_META_BYTE_ALIGNMENT: usize = 256;
/// Byte offset of the `receive_shadows` u32 inside the packed struct.
/// Matches the layout in `MaterialMeshMeta::to_bytes` and the
/// `receive_shadows` field in `shared_wgsl/material_mesh_meta.wgsl`.
/// Used by `MeshMeta::set_receive_shadows` for the in-place patch
/// path so the shadow toggle doesn't need to re-pack the entire
/// struct (which would require Materials/Transforms/Morphs context).
pub const MATERIAL_MESH_META_RECEIVE_SHADOWS_OFFSET: usize = 17 * 4;

pub static MATERIAL_BUFFER_USAGE: LazyLock<BufferUsage> = LazyLock::new(|| {
    BufferUsage::new()
        .with_copy_dst()
        .with_storage()
        .with_uniform()
});

/// Material meta fields used by shaders.
/// See `meta.wgsl` for the corresponding struct.
pub struct MaterialMeshMeta<'a> {
    pub mesh_key: MeshKey,
    pub material_key: MaterialKey,
    pub material_morph_key: Option<MaterialMorphKey>,
    pub custom_attribute_indices_offset: usize,
    pub custom_attribute_data_offset: usize,
    pub visibility_geometry_data_offset: Option<usize>,
    pub transform_offset: usize,
    pub normal_matrix_offset: usize,
    pub buffer_info: &'a MeshBufferInfo,
    pub materials: &'a Materials,
    pub morphs: &'a Morphs,
    pub mesh: &'a Mesh,
}

/// Calculate the offset (in floats) to TEXCOORD_0 within the vertex attribute data.
/// This accounts for any COLOR_n attributes that come before texture coordinates.
fn calculate_uv_sets_index(buffer_info: &MeshBufferInfo) -> u32 {
    let mut offset_floats = 0;
    for attr in &buffer_info.triangles.vertex_attributes {
        if let MeshBufferVertexAttributeInfo::Custom(custom) = attr {
            match custom {
                MeshBufferCustomVertexAttributeInfo::Colors { .. } => {
                    // vertex_size() returns bytes, divide by 4 to get float count
                    offset_floats += attr.vertex_size() / 4;
                }
                MeshBufferCustomVertexAttributeInfo::TexCoords { .. } => {
                    // Found TexCoords, stop counting
                    break;
                }
            }
        }
    }
    offset_floats as u32
}

/// Calculate how many UV sets and color sets this mesh has.
/// Returns (uv_set_count, color_set_count).
fn calculate_attribute_counts(buffer_info: &MeshBufferInfo) -> (u32, u32) {
    let mut uv_set_count = 0u32;
    let mut color_set_count = 0u32;

    for attr in &buffer_info.triangles.vertex_attributes {
        if let MeshBufferVertexAttributeInfo::Custom(custom) = attr {
            match custom {
                MeshBufferCustomVertexAttributeInfo::TexCoords { index, .. } => {
                    uv_set_count = uv_set_count.max(*index + 1);
                }
                MeshBufferCustomVertexAttributeInfo::Colors { index, .. } => {
                    color_set_count = color_set_count.max(*index + 1);
                }
            }
        }
    }

    (uv_set_count, color_set_count)
}

impl<'a> MaterialMeshMeta<'a> {
    /// Packs material meta into bytes.
    pub fn to_bytes(
        self,
    ) -> std::result::Result<[u8; MATERIAL_MESH_META_BYTE_SIZE], AwsmMeshError> {
        let Self {
            mesh_key,
            material_key,
            material_morph_key,
            buffer_info,
            custom_attribute_indices_offset,
            custom_attribute_data_offset,
            visibility_geometry_data_offset,
            transform_offset,
            normal_matrix_offset,
            materials,
            morphs,
            mesh,
        } = self;

        let mut result = [0u8; MATERIAL_MESH_META_BYTE_SIZE];
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

        // Morph (20 bytes)
        if let Some(morph_key) = material_morph_key {
            let info = morphs.material.get_info(morph_key)?;
            push_u32(info.targets_len as u32);
            push_u32(morphs.material.weights_buffer_offset(morph_key)? as u32);
            push_u32(morphs.material.values_buffer_offset(morph_key)? as u32);
            let mut bitmask = 0;
            if info.attributes.normal {
                bitmask |= MATERIAL_MESH_META_MORPH_MATERIAL_BITMASK_NORMAL;
            }
            if info.attributes.tangent {
                bitmask |= MATERIAL_MESH_META_MORPH_MATERIAL_BITMASK_TANGENT;
            }
            push_u32(bitmask);
        } else {
            push_u32(0);
            push_u32(0);
            push_u32(0);
            push_u32(0);
        }

        // Material (4 bytes)
        push_u32(materials.buffer_offset(material_key)? as u32);

        // Transform offset (4 bytes)
        push_u32(transform_offset as u32);
        // Normal matrix offset (4 bytes)
        push_u32(normal_matrix_offset as u32);

        // Vertex attribute offsets (8 bytes)
        push_u32(custom_attribute_indices_offset as u32);
        push_u32(custom_attribute_data_offset as u32);

        // Vertex attribute stride (4 bytes)
        push_u32(buffer_info.triangles.vertex_attribute_stride() as u32);

        // UV sets index - offset in floats to TEXCOORD_0 within vertex attribute data (4 bytes)
        let uv_sets_index = calculate_uv_sets_index(buffer_info);
        push_u32(uv_sets_index);

        // UV set count and color set count (8 bytes)
        let (uv_set_count, color_set_count) = calculate_attribute_counts(buffer_info);
        push_u32(uv_set_count);
        push_u32(color_set_count);

        // Geometry data offset (4 bytes)
        push_u32(visibility_geometry_data_offset.unwrap_or_default() as u32);

        // is hud
        push_u32(if mesh.hud { 1 } else { 0 });

        // receive_shadows — consumed by `apply_lighting` in
        // `lights.wgsl` to skip the shadow modulation when the mesh
        // opted out. Matches the `receive_shadows` u32 in
        // `material_mesh_meta.wgsl`.
        push_u32(if mesh.receive_shadows { 1 } else { 0 });

        Ok(result)
    }
}
