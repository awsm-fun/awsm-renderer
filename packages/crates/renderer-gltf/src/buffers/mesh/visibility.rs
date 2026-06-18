use std::{borrow::Cow, collections::BTreeMap};

use super::Result;
// FrontFace is only used by the now-`#[cfg(test)]` packer-parity writers below;
// the live source decoders (`resolve_attribute_buffers` / `decode_*`) don't need it.
#[cfg(test)]
use awsm_renderer_core::pipeline::primitive::FrontFace;

use awsm_renderer::meshes::buffer_info::{
    MeshBufferVertexAttributeInfo, MeshBufferVisibilityVertexAttributeInfo,
};

// The buffer-reader helpers are only used by the test-only reference writer.
#[cfg(test)]
use crate::buffers::mesh::{get_position_from_buffer, get_vec3_from_buffer, get_vec4_from_buffer};
use crate::error::AwsmGltfError;

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
///
/// No longer called on the live decode path (the renderer packs visibility bytes
/// at commit via the same `mesh_pack::pack_visibility_bytes`); retained as the
/// `#[cfg(test)]` parity check that the decode-side decode + canonical packer
/// reproduce the historical byte layout.
#[cfg(test)]
pub(super) fn create_visibility_vertices(
    attribute_data: &BTreeMap<MeshBufferVertexAttributeInfo, Cow<'_, [u8]>>,
    triangle_indices: &[[usize; 3]],
    front_face: FrontFace,
    visibility_vertex_bytes: &mut Vec<u8>,
) -> Result<()> {
    let (positions, normals, tangents) = resolve_attribute_buffers(attribute_data)?;

    // Decode the LE byte streams into typed slices (bit-preserving:
    // `from_le_bytes` → `to_le_bytes` round-trips every f32 bit pattern,
    // including NaNs) and delegate to the renderer's CANONICAL packer —
    // the Phase-2b convergence: one byte-layout definition for both upload
    // front-ends. Byte-identity with the previous hand-rolled writer is
    // pinned by the proptest below against `reference_visibility_vertices`.
    let positions_t = decode_vec3s(positions);
    let normals_t = decode_vec3s(normals);
    let tangents_t = tangents.map(decode_vec4s);

    // Bounds pre-check (the packer indexes unchecked; the old writer returned
    // an error on an out-of-range vertex — keep that contract).
    let limit = positions_t
        .len()
        .min(normals_t.len())
        .min(tangents_t.as_ref().map(|t| t.len()).unwrap_or(usize::MAX));
    let mut flat: Vec<u32> = Vec::with_capacity(triangle_indices.len() * 3);
    for tri in triangle_indices {
        for &v in tri {
            if v >= limit {
                return Err(AwsmGltfError::AttributeData(format!(
                    "vertex index {v} out of range (attribute count {limit})"
                )));
            }
            flat.push(v as u32);
        }
    }

    visibility_vertex_bytes.extend_from_slice(&awsm_renderer::mesh_pack::pack_visibility_bytes(
        &positions_t,
        &normals_t,
        tangents_t.as_deref(),
        &flat,
        front_face,
    ));
    Ok(())
}

/// `(positions, normals, tangents)` byte slices for a primitive.
pub(super) type AttributeBuffers<'a> = (&'a [u8], &'a [u8], Option<&'a [u8]>);

/// Positions / normals / optional tangents byte slices out of the attribute
/// map, with the format validations the writers rely on.
pub(super) fn resolve_attribute_buffers<'a>(
    attribute_data: &'a BTreeMap<MeshBufferVertexAttributeInfo, Cow<'_, [u8]>>,
) -> Result<AttributeBuffers<'a>> {
    let positions = attribute_data
        .iter()
        .find_map(|(attr_info, data)| match attr_info {
            MeshBufferVertexAttributeInfo::Visibility(
                MeshBufferVisibilityVertexAttributeInfo::Positions { .. },
            ) => Some(&data[..]),
            _ => None,
        })
        .ok_or_else(|| AwsmGltfError::Positions("missing positions".to_string()))?;
    let normals = attribute_data
        .iter()
        .find_map(|(attr_info, data)| match attr_info {
            MeshBufferVertexAttributeInfo::Visibility(
                MeshBufferVisibilityVertexAttributeInfo::Normals { .. },
            ) => Some(&data[..]),
            _ => None,
        })
        .ok_or_else(|| AwsmGltfError::AttributeData("missing normals".to_string()))?;
    let tangents = attribute_data
        .iter()
        .find_map(|(attr_info, data)| match attr_info {
            MeshBufferVertexAttributeInfo::Visibility(
                MeshBufferVisibilityVertexAttributeInfo::Tangents { .. },
            ) => Some(&data[..]),
            _ => None,
        });
    if positions.len() % 12 != 0 {
        return Err(AwsmGltfError::Positions(format!(
            "Position buffer length ({}) is not a multiple of 12 (3 * f32).",
            positions.len()
        )));
    }
    if normals.len() % 12 != 0 {
        return Err(AwsmGltfError::AttributeData(format!(
            "Normal buffer length ({}) is not a multiple of 12 (3 * f32).",
            normals.len()
        )));
    }
    if let Some(tangents) = tangents {
        if tangents.len() % 16 != 0 {
            return Err(AwsmGltfError::AttributeData(format!(
                "Tangent buffer length ({}) is not a multiple of 16 (4 * f32).",
                tangents.len()
            )));
        }
    }
    Ok((positions, normals, tangents))
}

/// LE byte stream → `[f32; 3]`s (bit-preserving).
pub(super) fn decode_vec3s(bytes: &[u8]) -> Vec<[f32; 3]> {
    bytes
        .chunks_exact(12)
        .map(|c| {
            [
                f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                f32::from_le_bytes([c[8], c[9], c[10], c[11]]),
            ]
        })
        .collect()
}

/// LE byte stream → `[f32; 4]`s (bit-preserving).
pub(super) fn decode_vec4s(bytes: &[u8]) -> Vec<[f32; 4]> {
    bytes
        .chunks_exact(16)
        .map(|c| {
            [
                f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                f32::from_le_bytes([c[8], c[9], c[10], c[11]]),
                f32::from_le_bytes([c[12], c[13], c[14], c[15]]),
            ]
        })
        .collect()
}

/// The pre-Phase-2b hand-rolled writer, kept as the byte-identity REFERENCE
/// the proptest compares the mesh_pack delegation against. Do not call from
/// live code.
#[cfg(test)]
pub(super) fn reference_visibility_vertices(
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

#[cfg(test)]
mod parity_tests {
    use std::borrow::Cow;
    use std::collections::BTreeMap;

    use proptest::prelude::*;

    use super::*;

    /// Build the attribute map the live entry points consume, from raw typed
    /// vertices (encoded LE — exactly what the gltf accessor lowering yields).
    fn attribute_map(
        positions: &[[f32; 3]],
        normals: &[[f32; 3]],
        tangents: Option<&[[f32; 4]]>,
    ) -> BTreeMap<MeshBufferVertexAttributeInfo, Cow<'static, [u8]>> {
        let mut map: BTreeMap<MeshBufferVertexAttributeInfo, Cow<'static, [u8]>> = BTreeMap::new();
        let enc3 = |vs: &[[f32; 3]]| -> Vec<u8> {
            vs.iter()
                .flat_map(|v| v.iter().flat_map(|f| f.to_le_bytes()))
                .collect()
        };
        map.insert(
            MeshBufferVertexAttributeInfo::Visibility(
                MeshBufferVisibilityVertexAttributeInfo::Positions {
                    data_size: 4,
                    component_len: 3,
                },
            ),
            Cow::Owned(enc3(positions)),
        );
        map.insert(
            MeshBufferVertexAttributeInfo::Visibility(
                MeshBufferVisibilityVertexAttributeInfo::Normals {
                    data_size: 4,
                    component_len: 3,
                },
            ),
            Cow::Owned(enc3(normals)),
        );
        if let Some(tangents) = tangents {
            map.insert(
                MeshBufferVertexAttributeInfo::Visibility(
                    MeshBufferVisibilityVertexAttributeInfo::Tangents {
                        data_size: 4,
                        component_len: 4,
                    },
                ),
                Cow::Owned(
                    tangents
                        .iter()
                        .flat_map(|v| v.iter().flat_map(|f| f.to_le_bytes()))
                        .collect::<Vec<u8>>(),
                ),
            );
        }
        map
    }

    /// Arbitrary f32 BIT PATTERNS (incl. NaN/inf/denormals) — the packer must
    /// be bit-preserving, so the strategy space is all of u32.
    fn any_f32() -> impl Strategy<Value = f32> {
        any::<u32>().prop_map(f32::from_bits)
    }

    fn vec3s(len: usize) -> impl Strategy<Value = Vec<[f32; 3]>> {
        proptest::collection::vec([any_f32(), any_f32(), any_f32()], len)
    }

    fn vec4s(len: usize) -> impl Strategy<Value = Vec<[f32; 4]>> {
        proptest::collection::vec([any_f32(), any_f32(), any_f32(), any_f32()], len)
    }

    proptest! {
        /// Phase-2b byte identity: the mesh_pack delegation produces EXACTLY
        /// the bytes the pre-2b hand-rolled writer did, for both windings,
        /// with and without tangents, across arbitrary f32 bit patterns.
        #[test]
        fn visibility_delegation_matches_reference(
            (positions, normals, tangents, tris, cw) in (1usize..24).prop_flat_map(|vcount| (
                vec3s(vcount),
                vec3s(vcount),
                proptest::option::of(vec4s(vcount)),
                proptest::collection::vec([0..vcount, 0..vcount, 0..vcount], 0..32),
                any::<bool>(),
            )),
        ) {
            let map = attribute_map(&positions, &normals, tangents.as_deref());
            let front_face = if cw { FrontFace::Cw } else { FrontFace::Ccw };

            let mut new_bytes = Vec::new();
            create_visibility_vertices(&map, &tris, front_face, &mut new_bytes).unwrap();
            let mut ref_bytes = Vec::new();
            reference_visibility_vertices(&map, &tris, front_face, &mut ref_bytes).unwrap();
            prop_assert_eq!(new_bytes, ref_bytes);
        }

        /// Same identity for the transparency (non-exploded) stream.
        #[test]
        fn transparency_delegation_matches_reference(
            (positions, normals, tangents) in (1usize..48).prop_flat_map(|vcount| (
                vec3s(vcount),
                vec3s(vcount),
                proptest::option::of(vec4s(vcount)),
            )),
        ) {
            let map = attribute_map(&positions, &normals, tangents.as_deref());
            let index = crate::buffers::MeshBufferAttributeIndexInfoWithOffset {
                offset: 0,
                count: 0,
            };
            let mut new_bytes = Vec::new();
            super::super::transparency::create_transparency_vertices(
                &map, &index, &[], 0, FrontFace::Ccw, &mut new_bytes,
            )
            .unwrap();
            let mut ref_bytes = Vec::new();
            super::super::transparency::reference_transparency_vertices(
                &map, &index, &[], 0, FrontFace::Ccw, &mut ref_bytes,
            )
            .unwrap();
            prop_assert_eq!(new_bytes, ref_bytes);
        }
    }
}
