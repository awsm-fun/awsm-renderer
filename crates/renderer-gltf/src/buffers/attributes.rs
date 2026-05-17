use std::{borrow::Cow, collections::BTreeMap};

use gltf::{accessor::DataType, Semantic};

use awsm_renderer::{
    buffer::helpers::{i16_to_i32_vec, u16_to_u32_vec, u8_to_i16_vec, u8_to_u16_vec},
    meshes::buffer_info::{MeshBufferCustomVertexAttributeInfo, MeshBufferVertexAttributeInfo},
};

use crate::{
    buffers::accessor::accessor_to_bytes,
    error::{AwsmGltfError, Result},
};

// Helper function to load attribute data (similar to your existing code)
pub(super) fn load_attribute_data_by_kind<'a>(
    gltf_attributes: &[(gltf::Semantic, gltf::Accessor<'_>)],
    buffers: &'a [Vec<u8>],
) -> Result<BTreeMap<MeshBufferVertexAttributeInfo, Cow<'a, [u8]>>> {
    let mut attribute_data = BTreeMap::new();

    for (semantic, accessor) in gltf_attributes {
        let mut attribute_kind = match convert_attribute_kind(semantic, accessor) {
            Some(kind) => kind,
            None => continue, // Skip unsupported semantics
        };
        let bytes = accessor_to_bytes(accessor, buffers)?;

        // wgsl doesn't work with 16-bit, so we may need to convert to 32-bit
        // For normalized data, convert to F32 (0.0-1.0 range)
        // For non-normalized data, promote to larger integer type
        let final_bytes = match (accessor.data_type(), accessor.normalized()) {
            (DataType::U8, true) => {
                // Normalized U8: convert to F32 (divide by 255.0)
                attribute_kind.force_data_size(4); // F32 size
                let f32_bytes: Vec<u8> = bytes
                    .iter()
                    .flat_map(|&v| {
                        let normalized = v as f32 / 255.0;
                        normalized.to_le_bytes()
                    })
                    .collect();
                Cow::Owned(f32_bytes)
            }
            (DataType::U8, false) => {
                // Non-normalized U8: keep as-is (already 8-bit)
                bytes
            }
            (DataType::I8, true) => {
                // Normalized I8: convert to F32 (divide by 127.0, clamped to [-1.0, 1.0])
                attribute_kind.force_data_size(4); // F32 size
                let f32_bytes: Vec<u8> = bytes
                    .iter()
                    .flat_map(|&v| {
                        let normalized = (v as i8 as f32 / 127.0).max(-1.0);
                        normalized.to_le_bytes()
                    })
                    .collect();
                Cow::Owned(f32_bytes)
            }
            (DataType::I8, false) => {
                // Non-normalized I8: keep as-is (already 8-bit)
                bytes
            }
            (DataType::U16, true) => {
                // Normalized U16: convert to F32 (divide by 65535.0)
                attribute_kind.force_data_size(4); // F32 size
                let u16_values = u8_to_u16_vec(&bytes);
                let f32_bytes: Vec<u8> = u16_values
                    .iter()
                    .flat_map(|&v| {
                        let normalized = v as f32 / 65535.0;
                        normalized.to_le_bytes()
                    })
                    .collect();
                Cow::Owned(f32_bytes)
            }
            (DataType::U16, false) => {
                // Non-normalized U16: promote to U32
                attribute_kind.force_data_size(4);
                Cow::Owned(u16_to_u32_vec(&bytes))
            }
            (DataType::I16, true) => {
                // Normalized I16: convert to F32 (divide by 32767.0, clamped to [-1.0, 1.0])
                attribute_kind.force_data_size(4);
                let i16_values = u8_to_i16_vec(&bytes);
                let f32_bytes: Vec<u8> = i16_values
                    .iter()
                    .flat_map(|&v| {
                        let normalized = (v as f32 / 32767.0).max(-1.0);
                        normalized.to_le_bytes()
                    })
                    .collect();
                Cow::Owned(f32_bytes)
            }
            (DataType::I16, false) => {
                // Non-normalized I16: promote to I32
                attribute_kind.force_data_size(4);
                Cow::Owned(i16_to_i32_vec(&bytes))
            }
            (DataType::U32, _) | (DataType::F32, _) => {
                // U32 and F32: already correct size and format
                bytes
            }
        };

        attribute_data.insert(attribute_kind, final_bytes);
    }

    Ok(attribute_data)
}

// Pack vertex attributes in interleaved layout (for indexed access)
pub(super) fn pack_vertex_attributes(
    attribute_data: Vec<(&MeshBufferCustomVertexAttributeInfo, &Cow<'_, [u8]>)>,
    vertex_attribute_bytes: &mut Vec<u8>,
) -> Result<()> {
    if attribute_data.is_empty() {
        return Ok(());
    }

    // Determine vertex count (ensure all attributes have matching lengths).
    // This keeps silently truncated buffers from slipping through and causing
    // unpredictable out-of-bounds reads in the compute shaders later on.
    let vertex_count = attribute_data
        .iter()
        .map(|(attr_info, attr_data)| {
            let stride = attr_info.vertex_size();
            debug_assert!(stride > 0);
            (attr_data.len() / stride, attr_data.len() % stride)
        })
        .try_fold(None, |acc, (count, remainder)| {
            if remainder != 0 {
                return Err(());
            }
            match acc {
                None => Ok(Some(count)),
                Some(prev) if prev == count => Ok(Some(prev)),
                Some(_) => Err(()),
            }
        })
        .map_err(|_| {
            AwsmGltfError::AttributeData(
                "vertex attribute buffers do not share a common vertex count".to_string(),
            )
        })?
        .unwrap_or(0);

    for vertex_index in 0..vertex_count {
        for (attr_info, attr_data) in attribute_data.iter() {
            let stride = attr_info.vertex_size();
            let start = vertex_index * stride;
            let end = start + stride;
            // Copy one vertex worth of data per attribute, preserving the
            // attribute ordering enforced by the BTree map above.
            vertex_attribute_bytes.extend_from_slice(&attr_data[start..end]);
        }
    }

    Ok(())
}

pub(super) fn convert_attribute_kind(
    semantic: &gltf::Semantic,
    accessor: &gltf::Accessor<'_>,
) -> Option<MeshBufferVertexAttributeInfo> {
    use awsm_renderer::meshes::buffer_info::{
        MeshBufferCustomVertexAttributeInfo, MeshBufferVisibilityVertexAttributeInfo,
    };

    match semantic {
        Semantic::Positions => Some(MeshBufferVertexAttributeInfo::Visibility(
            MeshBufferVisibilityVertexAttributeInfo::Positions {
                data_size: accessor.data_type().size(),
                component_len: accessor.dimensions().multiplicity(),
            },
        )),
        Semantic::Normals => Some(MeshBufferVertexAttributeInfo::Visibility(
            MeshBufferVisibilityVertexAttributeInfo::Normals {
                data_size: accessor.data_type().size(),
                component_len: accessor.dimensions().multiplicity(),
            },
        )),
        Semantic::Tangents => Some(MeshBufferVertexAttributeInfo::Visibility(
            MeshBufferVisibilityVertexAttributeInfo::Tangents {
                data_size: accessor.data_type().size(),
                component_len: accessor.dimensions().multiplicity(),
            },
        )),
        Semantic::Colors(index) => Some(MeshBufferVertexAttributeInfo::Custom(
            MeshBufferCustomVertexAttributeInfo::Colors {
                data_size: accessor.data_type().size(),
                component_len: accessor.dimensions().multiplicity(),
                index: *index,
            },
        )),
        Semantic::TexCoords(index) => Some(MeshBufferVertexAttributeInfo::Custom(
            MeshBufferCustomVertexAttributeInfo::TexCoords {
                data_size: accessor.data_type().size(),
                component_len: accessor.dimensions().multiplicity(),
                index: *index,
            },
        )),
        Semantic::Joints(_) => {
            //extracted into storage buffer
            None
        }
        Semantic::Weights(_) => {
            // extracted into storage buffer
            None
        }
    }
}
