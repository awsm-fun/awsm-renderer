use std::borrow::Cow;

use crate::buffers::accessor::accessor_to_bytes;
use crate::buffers::MeshBufferSkinInfoWithOffset;
use crate::error::{AwsmGltfError, Result};
use awsm_renderer::buffer::helpers::{u8_to_f32_iter, u8_to_u16_iter, u8_to_u32_iter};

/// Converts GLTF skin data into storage buffers.
///
/// IMPORTANT: Skinning data (Joints/Weights) is NOT stored as vertex attributes.
/// It is stored in dedicated skin storage buffers and accessed by the geometry pass.
/// This separation ensures:
/// - Memory efficiency (no duplication)
/// - Clear architecture (skins ≠ attributes)
/// - Type safety (custom meshes can't accidentally add skin data as attributes)
///
/// INDEXED SKIN DATA:
/// - Skins are stored per original vertex (not exploded)
/// - One entry per original vertex containing joint indices/weights
/// - All data is standardized: indices as u32, weights as f32
/// - Shaders access via original vertex index from the index buffer
pub(super) fn convert_skin(
    primitive: &gltf::Primitive,
    buffers: &[Vec<u8>],
    vertex_count: usize, // NEW: Original vertex count instead of triangle/index data
    skin_joint_index_weight_bytes: &mut Vec<u8>, // Indices (u32) interleaved with weights (f32)
) -> Result<Option<MeshBufferSkinInfoWithOffset>> {
    // Check if we have any skin data
    let has_joints_0 = primitive.get(&gltf::Semantic::Joints(0)).is_some();
    let has_weights_0 = primitive.get(&gltf::Semantic::Weights(0)).is_some();

    if !has_joints_0 || !has_weights_0 {
        return Ok(None);
    }

    // Load all skin set data from GLTF (JOINTS_0/WEIGHTS_0, JOINTS_1/WEIGHTS_1, etc.)
    struct SkinSetData<'a> {
        joints_data: Cow<'a, [u8]>,
        joints_data_type: gltf::accessor::DataType,
        weights_data: Cow<'a, [u8]>,
        weights_data_type: gltf::accessor::DataType,
    }

    let mut skin_sets_data = Vec::new();
    let mut set_index = 0;

    // Collect all available skin sets
    loop {
        let joints_semantic = gltf::Semantic::Joints(set_index);
        let weights_semantic = gltf::Semantic::Weights(set_index);

        let joints_accessor = primitive.get(&joints_semantic);
        let weights_accessor = primitive.get(&weights_semantic);

        match (joints_accessor, weights_accessor) {
            (Some(joints_accessor), Some(weights_accessor)) => {
                let joints_data = accessor_to_bytes(&joints_accessor, buffers)?;
                let weights_data = accessor_to_bytes(&weights_accessor, buffers)?;

                skin_sets_data.push(SkinSetData {
                    joints_data,
                    joints_data_type: joints_accessor.data_type(),
                    weights_data,
                    weights_data_type: weights_accessor.data_type(),
                });

                set_index += 1;
            }
            _ => break, // No more skin sets
        }
    }

    if skin_sets_data.is_empty() {
        return Ok(None);
    }

    let set_count = skin_sets_data.len();
    let index_weights_offset = skin_joint_index_weight_bytes.len();

    // INDEXED SKIN DATA (per original vertex, not exploded)
    // Store one entry per original vertex
    // All data is standardized to u32 indices and f32 weights
    for vertex_index in 0..vertex_count {
        // For each skin set (interleaved per vertex)
        for skin_set_data in &skin_sets_data {
            // Convert and add joint indices (standardized to u32)
            let indices_u32 = convert_indices_to_u32(
                &skin_set_data.joints_data,
                skin_set_data.joints_data_type,
                vertex_index,
            )?;
            // Convert and add joint weights (standardized to f32)
            let weights_f32 = convert_weights_to_f32(
                &skin_set_data.weights_data,
                skin_set_data.weights_data_type,
                vertex_index,
            )?;

            for i in 0..4 {
                skin_joint_index_weight_bytes.extend_from_slice(&indices_u32[i].to_le_bytes());
                skin_joint_index_weight_bytes.extend_from_slice(&weights_f32[i].to_le_bytes());
            }
        }
    }

    let index_weights_size = skin_joint_index_weight_bytes.len() - index_weights_offset;

    Ok(Some(MeshBufferSkinInfoWithOffset {
        set_count,
        index_weights_offset,
        index_weights_size,
    }))
}

/// Converts joint indices from GLTF format to standardized u32
fn convert_indices_to_u32(
    data: &[u8],
    data_type: gltf::accessor::DataType,
    vertex_index: usize,
) -> Result<[u32; 4]> {
    let mut indices = [0u32; 4];

    match data_type {
        gltf::accessor::DataType::U16 => {
            let stride = 8; // vec4<u16>
            let offset = vertex_index * stride;
            for (i, value) in u8_to_u16_iter(&data[offset..]).take(4).enumerate() {
                indices[i] = value.into();
            }
        }
        gltf::accessor::DataType::U32 => {
            let stride = 16; // vec4<u32>
            let offset = vertex_index * stride;
            for (i, value) in u8_to_u32_iter(&data[offset..]).take(4).enumerate() {
                indices[i] = value;
            }
        }
        gltf::accessor::DataType::U8 => {
            let stride = 4; // vec4<u8>
            let offset = vertex_index * stride;
            for (i, value) in data.iter().skip(offset).take(4).enumerate() {
                indices[i] = (*value).into();
            }
        }
        _ => {
            return Err(AwsmGltfError::UnsupportedSkinDataType(data_type));
        }
    }

    Ok(indices)
}

/// Converts joint weights from GLTF format to standardized f32
fn convert_weights_to_f32(
    data: &[u8],
    data_type: gltf::accessor::DataType,
    vertex_index: usize,
) -> Result<[f32; 4]> {
    let mut weights = [0.0f32; 4];
    match data_type {
        gltf::accessor::DataType::F32 => {
            let stride = 16; // vec4<f32>
            let offset = vertex_index * stride;
            for (i, value) in u8_to_f32_iter(&data[offset..]).take(4).enumerate() {
                weights[i] = value;
            }
        }
        gltf::accessor::DataType::U16 => {
            let stride = 8; // vec4<u16>
            let offset = vertex_index * stride;
            for (i, value) in u8_to_u16_iter(&data[offset..]).take(4).enumerate() {
                // Convert normalized u16 to f32 (0-65535 → 0.0-1.0)
                weights[i] = value as f32 / 65535.0;
            }
        }
        gltf::accessor::DataType::U8 => {
            let stride = 4; // vec4<u8>
            let offset = vertex_index * stride;
            for (i, value) in data.iter().skip(offset).take(4).enumerate() {
                // Convert normalized u8 to f32 (0-255 → 0.0-1.0)
                weights[i] = *value as f32 / 255.0;
            }
        }
        _ => {
            return Err(AwsmGltfError::SkinWeights(format!(
                "Unsupported joint weight data type: {:?}",
                data_type
            )));
        }
    }

    Ok(weights)
}
