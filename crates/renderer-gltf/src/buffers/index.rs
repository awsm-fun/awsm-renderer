//! Index buffer extraction helpers for glTF.

use crate::buffers::{accessor::accessor_to_bytes, MeshBufferAttributeIndexInfoWithOffset};
use crate::error::{AwsmGltfError, Result};

/// Index buffer info extracted from a glTF primitive.
#[derive(Debug, Clone)]
pub struct GltfMeshBufferIndexInfo {
    // offset in index_bytes where this primitive starts
    pub offset: usize,
    // number of index elements for this primitive
    pub count: usize,
}

impl GltfMeshBufferIndexInfo {
    /// Returns total byte size if it fits in `usize`.
    pub fn checked_total_size(&self) -> Option<usize> {
        self.count.checked_mul(4) // guaranteed u32
    }

    // the size in bytes of the index buffer for this primitive
    /// Returns the total size in bytes for this index buffer.
    pub fn total_size(&self) -> usize {
        self.checked_total_size()
            .expect("index byte size overflowed usize")
    }
}

impl From<GltfMeshBufferIndexInfo> for MeshBufferAttributeIndexInfoWithOffset {
    fn from(info: GltfMeshBufferIndexInfo) -> Self {
        Self {
            offset: info.offset,
            count: info.count,
        }
    }
}

impl GltfMeshBufferIndexInfo {
    /// Creates index info from a primitive if indices are provided.
    pub fn maybe_new(
        primitive: &gltf::Primitive<'_>,
        buffers: &[Vec<u8>],
        index_bytes: &mut Vec<u8>,
    ) -> Result<Option<Self>> {
        match primitive.indices() {
            None => Ok(None),
            Some(accessor) => {
                let offset = index_bytes.len();
                let accessor_bytes = accessor_to_bytes(&accessor, buffers)?;
                let required_bytes = accessor.count().checked_mul(4).ok_or_else(|| {
                    AwsmGltfError::ExtractIndices("Index byte size overflowed usize".to_string())
                })?;
                index_bytes.reserve(required_bytes);

                match accessor.data_type() {
                    gltf::accessor::DataType::U32 => {
                        index_bytes.extend_from_slice(&accessor_bytes);
                    }

                    gltf::accessor::DataType::U16 => {
                        for bytes in accessor_bytes.chunks_exact(2) {
                            let value = u16::from_le_bytes([bytes[0], bytes[1]]);
                            index_bytes.extend_from_slice(&u32::from(value).to_le_bytes());
                        }
                    }

                    gltf::accessor::DataType::I16 => {
                        for bytes in accessor_bytes.chunks_exact(2) {
                            let value = i16::from_le_bytes([bytes[0], bytes[1]]);
                            if value < 0 {
                                return Err(AwsmGltfError::NegativeIndexValue(value as i32));
                            }
                            index_bytes.extend_from_slice(&u32::try_from(value)?.to_le_bytes());
                        }
                    }
                    gltf::accessor::DataType::I8 => {
                        for byte in accessor_bytes.iter() {
                            let value = *byte as i8;
                            if value < 0 {
                                return Err(AwsmGltfError::NegativeIndexValue(value as i32));
                            }
                            index_bytes.extend_from_slice(&u32::try_from(value)?.to_le_bytes());
                        }
                    }
                    gltf::accessor::DataType::U8 => {
                        for value in accessor_bytes.iter() {
                            index_bytes.extend_from_slice(&u32::from(*value).to_le_bytes());
                        }
                    }

                    gltf::accessor::DataType::F32 => {
                        for bytes in accessor_bytes.chunks_exact(4) {
                            let value =
                                f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                            let value = value as u32;
                            index_bytes.extend_from_slice(&value.to_le_bytes());
                        }
                    }
                }

                let info = Self {
                    offset,
                    count: accessor.count(),
                };

                assert_eq!(index_bytes.len() - offset, required_bytes);

                Ok(Some(info))
            }
        }
    }
}

/// Generates sequential indices for a primitive without indices.
pub fn generate_fresh_indices_from_primitive(
    primitive: &gltf::Primitive,
    index_bytes: &mut Vec<u8>,
) -> Result<MeshBufferAttributeIndexInfoWithOffset> {
    let offset = index_bytes.len();
    // Get vertex count from any attribute (positions is guaranteed to exist)
    let vertex_count = primitive
        .attributes()
        .next()
        .map(|(_, accessor)| accessor.count())
        .unwrap_or(0);

    if vertex_count == 0 {
        return Ok(MeshBufferAttributeIndexInfoWithOffset { offset, count: 0 });
    }

    // Generate sequential indices based on primitive mode
    match primitive.mode() {
        gltf::mesh::Mode::Triangles => {
            // Simple case: 0, 1, 2, 3, 4, 5, ...
            let index_count = vertex_count;
            for i in 0..index_count {
                index_bytes.extend_from_slice(&(i as u32).to_le_bytes());
            }

            Ok(MeshBufferAttributeIndexInfoWithOffset {
                offset,
                count: index_count,
            })
        }
        gltf::mesh::Mode::TriangleStrip => {
            // Convert triangle strip to triangle list
            if vertex_count < 3 {
                return Err(AwsmGltfError::UnsupportedIndexMode(
                    "Triangle strip needs at least 3 vertices".to_string(),
                ));
            }

            let triangle_count = vertex_count - 2;
            let index_count = triangle_count * 3;

            for i in 0..triangle_count {
                let (v0, v1, v2) = if i % 2 == 0 {
                    // Even triangles: normal winding
                    (i, i + 1, i + 2)
                } else {
                    // Odd triangles: reverse winding to maintain consistent orientation
                    (i, i + 2, i + 1)
                };

                index_bytes.extend_from_slice(&(v0 as u32).to_le_bytes());
                index_bytes.extend_from_slice(&(v1 as u32).to_le_bytes());
                index_bytes.extend_from_slice(&(v2 as u32).to_le_bytes());
            }

            Ok(MeshBufferAttributeIndexInfoWithOffset {
                offset,
                count: index_count,
            })
        }
        gltf::mesh::Mode::TriangleFan => {
            // Convert triangle fan to triangle list
            if vertex_count < 3 {
                return Err(AwsmGltfError::UnsupportedIndexMode(
                    "Triangle fan needs at least 3 vertices".to_string(),
                ));
            }

            let triangle_count = vertex_count - 2;
            let index_count = triangle_count * 3;

            for i in 0..triangle_count {
                // Fan triangles: (0, i+1, i+2)
                let (v0, v1, v2) = (0, i + 1, i + 2);

                index_bytes.extend_from_slice(&(v0 as u32).to_le_bytes());
                index_bytes.extend_from_slice(&(v1 as u32).to_le_bytes());
                index_bytes.extend_from_slice(&(v2 as u32).to_le_bytes());
            }

            Ok(MeshBufferAttributeIndexInfoWithOffset {
                offset,
                count: index_count,
            })
        }
        other => Err(AwsmGltfError::UnsupportedIndexMode(format!(
            "Primitive mode {:?} not supported for visibility buffer rendering",
            other
        ))),
    }
}

pub(super) fn extract_triangle_indices(
    index: &MeshBufferAttributeIndexInfoWithOffset,
    all_index_bytes: &[u8],
) -> Result<Vec<[usize; 3]>> {
    if index.count % 3 != 0 {
        return Err(AwsmGltfError::ExtractIndices(format!(
            "Index count ({}) is not a multiple of 3, cannot form triangles.",
            index.count
        )));
    }

    if index.count == 0 {
        return Ok(Vec::new());
    }

    let byte_size = index.checked_total_size().ok_or_else(|| {
        AwsmGltfError::ExtractIndices("Index byte size overflowed usize".to_string())
    })?;
    let end = index.offset.checked_add(byte_size).ok_or_else(|| {
        AwsmGltfError::ExtractIndices("Index byte range overflowed usize".to_string())
    })?;
    if end > all_index_bytes.len() {
        return Err(AwsmGltfError::ExtractIndices(format!(
            "Index byte range [{}..{}) exceeds buffer length {}",
            index.offset,
            end,
            all_index_bytes.len()
        )));
    }

    // we're just working with the bytes of this primitive
    let index_bytes = &all_index_bytes[index.offset..end];

    let num_triangles = index.count / 3;
    let expected_bytes = num_triangles.checked_mul(12).ok_or_else(|| {
        AwsmGltfError::ExtractIndices("Triangle byte count overflowed usize".to_string())
    })?;
    if index_bytes.len() != expected_bytes {
        return Err(AwsmGltfError::ExtractIndices(format!(
            "Index byte length mismatch: expected {}, got {}",
            expected_bytes,
            index_bytes.len()
        )));
    }

    let mut triangles = Vec::with_capacity(num_triangles);

    for triangle_bytes in index_bytes.chunks_exact(12) {
        // IMPORTANT: vertex indices always reference the original per-vertex attribute arrays
        // (positions, normals, texcoords, etc), before any triangle explosion.
        let i0 = u32::from_le_bytes(triangle_bytes[0..4].try_into().unwrap()) as usize;
        let i1 = u32::from_le_bytes(triangle_bytes[4..8].try_into().unwrap()) as usize;
        let i2 = u32::from_le_bytes(triangle_bytes[8..12].try_into().unwrap()) as usize;
        triangles.push([i0, i1, i2]);
    }

    // Return array of triangles, where each triangle is [vertex_idx_0, vertex_idx_1, vertex_idx_2]
    // These indices reference the ORIGINAL per-vertex attribute data
    //
    // NEXT STEP: The visibility buffer conversion will use these triangles to:
    // 1. Look up vertex attributes (positions, normals, etc.) using these indices
    // 2. "Explode" the triangles by creating 3 separate vertices per triangle
    // 3. Generate NEW sequential indices (0,1,2,3,4,5...) for the exploded vertices
    // 4. Assign triangle IDs and barycentric coordinates to each exploded vertex
    Ok(triangles)
}
