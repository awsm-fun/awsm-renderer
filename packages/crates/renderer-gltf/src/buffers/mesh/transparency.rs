use std::{borrow::Cow, collections::BTreeMap};

use super::Result;
use awsm_renderer_core::pipeline::primitive::FrontFace;

use awsm_renderer::meshes::buffer_info::{
    MeshBufferVertexAttributeInfo, MeshBufferVisibilityVertexAttributeInfo,
};

use crate::{
    buffers::{mesh::get_vec3_from_buffer, MeshBufferAttributeIndexInfoWithOffset},
    error::AwsmGltfError,
};

/// Creates NON-EXPLODED transparency vertices for traditional forward rendering.
///
/// Unlike visibility buffer rendering (which requires vertex explosion), transparency uses
/// traditional forward rendering where vertices can be shared between triangles using an
/// index buffer. The GPU handles attribute interpolation automatically - no need for
/// triangle_index or barycentric coordinates.
///
/// Example: A cube with 8 vertices and 12 triangles stays as 8 vertices (NOT exploded to 36).
///
/// Each output vertex contains:
/// - Position (vec3<f32>): 12 bytes - from original GLTF vertex
/// - Normal (vec3<f32>): 12 bytes - from original GLTF vertex (preserves smooth/hard edges)
/// - Tangent (vec4<f32>): 16 bytes - from original GLTF vertex (or default if missing)
/// - Total: 40 bytes per vertex
///
/// This is used with the original index buffer (with vertex sharing) for efficient rendering.
/// Additional attributes (UVs, colors, etc.) can be added to this vertex format in the future.
pub(super) fn create_transparency_vertices(
    attribute_data: &BTreeMap<MeshBufferVertexAttributeInfo, Cow<'_, [u8]>>,
    _index: &MeshBufferAttributeIndexInfoWithOffset,
    _index_bytes: &[u8],
    _triangle_count: usize,
    _front_face: FrontFace,
    transparency_vertex_bytes: &mut Vec<u8>,
) -> Result<()> {
    // Get positions data
    let positions = attribute_data
        .iter()
        .find_map(|(attr_info, data)| match attr_info {
            MeshBufferVertexAttributeInfo::Visibility(
                MeshBufferVisibilityVertexAttributeInfo::Positions { .. },
            ) => Some(&data[..]),
            _ => None,
        })
        .ok_or_else(|| AwsmGltfError::Positions("missing positions".to_string()))?;

    // Get normals data (ensured to exist by ensure_normals() call)
    let normals = attribute_data
        .iter()
        .find_map(|(attr_info, data)| match attr_info {
            MeshBufferVertexAttributeInfo::Visibility(
                MeshBufferVisibilityVertexAttributeInfo::Normals { .. },
            ) => Some(&data[..]),
            _ => None,
        })
        .ok_or_else(|| AwsmGltfError::AttributeData("missing normals".to_string()))?;

    // Get tangents data (optional)
    let tangents = attribute_data
        .iter()
        .find_map(|(attr_info, data)| match attr_info {
            MeshBufferVertexAttributeInfo::Visibility(
                MeshBufferVisibilityVertexAttributeInfo::Tangents { .. },
            ) => Some(&data[..]),
            _ => None,
        });

    // Validate positions buffer (must be Float32x3 format)
    if positions.len() % 12 != 0 {
        return Err(AwsmGltfError::Positions(format!(
            "Position buffer length ({}) is not a multiple of 12 (3 * f32).",
            positions.len()
        )));
    }

    // Validate normals buffer (must be Float32x3 format)
    if normals.len() % 12 != 0 {
        return Err(AwsmGltfError::AttributeData(format!(
            "Normal buffer length ({}) is not a multiple of 12 (3 * f32).",
            normals.len()
        )));
    }

    // Validate tangents buffer if present (must be Float32x4 format)
    if let Some(tangents) = tangents {
        if tangents.len() % 16 != 0 {
            return Err(AwsmGltfError::AttributeData(format!(
                "Tangent buffer length ({}) is not a multiple of 16 (4 * f32).",
                tangents.len()
            )));
        }
    }

    // Calculate vertex count from positions buffer
    let vertex_count = positions.len() / 12;

    // NO EXPLOSION: Process each original vertex once
    // This maintains the indexed structure - vertices are shared between triangles
    for vertex_index in 0..vertex_count {
        // Get position for this vertex
        let position = get_vec3_from_buffer(positions, vertex_index, "position")?;

        // Get normal for this vertex
        let normal = get_vec3_from_buffer(normals, vertex_index, "normal")?;

        // Get tangent for this vertex (or default to [0, 0, 0, 1])
        let tangent = if let Some(tangents) = tangents {
            [
                f32::from_le_bytes([
                    tangents[vertex_index * 16],
                    tangents[vertex_index * 16 + 1],
                    tangents[vertex_index * 16 + 2],
                    tangents[vertex_index * 16 + 3],
                ]),
                f32::from_le_bytes([
                    tangents[vertex_index * 16 + 4],
                    tangents[vertex_index * 16 + 5],
                    tangents[vertex_index * 16 + 6],
                    tangents[vertex_index * 16 + 7],
                ]),
                f32::from_le_bytes([
                    tangents[vertex_index * 16 + 8],
                    tangents[vertex_index * 16 + 9],
                    tangents[vertex_index * 16 + 10],
                    tangents[vertex_index * 16 + 11],
                ]),
                f32::from_le_bytes([
                    tangents[vertex_index * 16 + 12],
                    tangents[vertex_index * 16 + 13],
                    tangents[vertex_index * 16 + 14],
                    tangents[vertex_index * 16 + 15],
                ]),
            ]
        } else {
            [0.0, 0.0, 0.0, 1.0] // Default tangent
        };

        // Write vertex data: position (12) + normal (12) + tangent (16) = 40 bytes
        // NO triangle_index, NO barycentric - not needed for forward rendering!

        // Position (12 bytes)
        transparency_vertex_bytes.extend_from_slice(&position[0].to_le_bytes());
        transparency_vertex_bytes.extend_from_slice(&position[1].to_le_bytes());
        transparency_vertex_bytes.extend_from_slice(&position[2].to_le_bytes());

        // Normal (12 bytes)
        transparency_vertex_bytes.extend_from_slice(&normal[0].to_le_bytes());
        transparency_vertex_bytes.extend_from_slice(&normal[1].to_le_bytes());
        transparency_vertex_bytes.extend_from_slice(&normal[2].to_le_bytes());

        // Tangent (16 bytes)
        transparency_vertex_bytes.extend_from_slice(&tangent[0].to_le_bytes());
        transparency_vertex_bytes.extend_from_slice(&tangent[1].to_le_bytes());
        transparency_vertex_bytes.extend_from_slice(&tangent[2].to_le_bytes());
        transparency_vertex_bytes.extend_from_slice(&tangent[3].to_le_bytes());
    }

    Ok(())
}
