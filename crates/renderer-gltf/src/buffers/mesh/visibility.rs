use std::{borrow::Cow, collections::BTreeMap};

use super::Result;
use awsm_renderer_core::pipeline::primitive::FrontFace;

use awsm_renderer::meshes::buffer_info::{
    MeshBufferVertexAttributeInfo, MeshBufferVisibilityVertexAttributeInfo,
};

use crate::{
    buffers::mesh::{get_position_from_buffer, get_vec3_from_buffer, get_vec4_from_buffer},
    error::AwsmGltfError,
};

/// Creates EXPLODED visibility vertices for deferred/visibility buffer rendering.
///
/// This function performs "vertex explosion" - converting shared/indexed vertices into
/// per-triangle-vertex data. This is necessary for deferred rendering because each vertex
/// needs to carry per-triangle metadata (triangle_index and barycentric coordinates) that
/// cannot be shared between triangles.
///
/// Example: A cube with 8 vertices and 12 triangles becomes 36 vertices (12 * 3).
///
/// Each output vertex contains:
/// - Position (vec3<f32>): 12 bytes - copied from original GLTF vertex
/// - Triangle Index (u32): 4 bytes - unique per triangle (why explosion is needed!)
/// - Barycentric (vec2<f32>): 8 bytes - unique per corner (why explosion is needed!)
/// - Normal (vec3<f32>): 12 bytes - copied from original GLTF vertex (preserves smooth/hard edges)
/// - Tangent (vec4<f32>): 16 bytes - copied from original GLTF vertex
/// - Original Vertex Index (u32): 4 bytes - for indexed skin/morph access
/// - Total: 56 bytes per vertex
///
/// The explosion preserves GLTF's original normals:
/// - Smooth edges: GLTF shared vertices with averaged normals → same normal copied to all 3 corners → smooth shading preserved
/// - Hard edges: GLTF duplicated vertices with different normals → respective normals copied → hard edges preserved
pub(super) fn create_visibility_vertices(
    attribute_data: &BTreeMap<MeshBufferVertexAttributeInfo, Cow<'_, [u8]>>,
    triangle_indices: &[[usize; 3]],
    front_face: FrontFace,
    visibility_vertex_bytes: &mut Vec<u8>,
) -> Result<()> {
    static BARYCENTRICS: [[f32; 2]; 3] = [
        [1.0, 0.0], // First vertex: (1, 0, 0) - z = 1-1-0 = 0
        [0.0, 1.0], // Second vertex: (0, 1, 0) - z = 1-0-1 = 0
        [0.0, 0.0], // Third vertex: (0, 0, 1) - z = 1-0-0 = 1
    ];

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

    // VERTEX EXPLOSION: Process each triangle and create 3 separate vertices per triangle
    // This is necessary because each vertex needs unique triangle_index and barycentric values
    for (triangle_index, triangle) in triangle_indices.iter().enumerate() {
        let vertex_indices = match front_face {
            FrontFace::Cw => [triangle[0], triangle[2], triangle[1]],
            _ => [triangle[0], triangle[1], triangle[2]],
        };

        let barycentrics = match front_face {
            FrontFace::Cw => [BARYCENTRICS[0], BARYCENTRICS[2], BARYCENTRICS[1]],
            _ => BARYCENTRICS,
        };

        // Create 3 EXPLODED vertices for this triangle (one per corner)
        // Each vertex gets unique triangle_index and barycentric, but copies position/normal/tangent from original
        for (bary, &vertex_index) in barycentrics.iter().zip(vertex_indices.iter()) {
            // Get position for this vertex
            let position = get_position_from_buffer(positions, vertex_index)?;

            // Get normal for this vertex
            let normal = get_vec3_from_buffer(normals, vertex_index, "normal")?;

            // Get tangent for this vertex (or default to [0, 0, 0, 1])
            let tangent = if let Some(tangents) = tangents {
                get_vec4_from_buffer(tangents, vertex_index, "tangent")?
            } else {
                [0.0, 0.0, 0.0, 1.0] // Default tangent
            };

            // Write vertex data: position (12) + triangle_index (4) + barycentric (8) + normal (12) + tangent (16) + original_vertex_index (4) = 56 bytes

            // Position (12 bytes)
            visibility_vertex_bytes.extend_from_slice(&position[0].to_le_bytes());
            visibility_vertex_bytes.extend_from_slice(&position[1].to_le_bytes());
            visibility_vertex_bytes.extend_from_slice(&position[2].to_le_bytes());

            // Triangle index (4 bytes)
            visibility_vertex_bytes.extend_from_slice(&(triangle_index as u32).to_le_bytes());

            // Barycentric coordinates (8 bytes)
            visibility_vertex_bytes.extend_from_slice(&bary[0].to_le_bytes());
            visibility_vertex_bytes.extend_from_slice(&bary[1].to_le_bytes());

            // Normal (12 bytes)
            visibility_vertex_bytes.extend_from_slice(&normal[0].to_le_bytes());
            visibility_vertex_bytes.extend_from_slice(&normal[1].to_le_bytes());
            visibility_vertex_bytes.extend_from_slice(&normal[2].to_le_bytes());

            // Tangent (16 bytes)
            visibility_vertex_bytes.extend_from_slice(&tangent[0].to_le_bytes());
            visibility_vertex_bytes.extend_from_slice(&tangent[1].to_le_bytes());
            visibility_vertex_bytes.extend_from_slice(&tangent[2].to_le_bytes());
            visibility_vertex_bytes.extend_from_slice(&tangent[3].to_le_bytes());

            // Original vertex index (4 bytes) - for indexed skin/morph access
            visibility_vertex_bytes.extend_from_slice(&(vertex_index as u32).to_le_bytes());
        }
    }

    Ok(())
}
