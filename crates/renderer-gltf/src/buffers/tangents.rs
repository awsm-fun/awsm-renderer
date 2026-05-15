use std::{borrow::Cow, collections::BTreeMap};

use awsm_renderer::meshes::buffer_info::{
    MeshBufferCustomVertexAttributeInfo, MeshBufferVertexAttributeInfo,
    MeshBufferVisibilityVertexAttributeInfo,
};

use crate::error::{AwsmGltfError, Result};

/// Generates tangents using MikkTSpace algorithm if:
/// - The primitive has a normal map (material has normalTexture)
/// - The primitive doesn't already have tangent attributes
/// - UV coordinates exist (required for tangent calculation)
pub(super) fn ensure_tangents<'a>(
    mut attribute_data: BTreeMap<MeshBufferVertexAttributeInfo, Cow<'a, [u8]>>,
    primitive: &gltf::Primitive<'_>,
    triangle_indices: &[[usize; 3]],
) -> Result<BTreeMap<MeshBufferVertexAttributeInfo, Cow<'a, [u8]>>> {
    // Check if tangents already exist
    let has_tangents = attribute_data.keys().any(|x| {
        matches!(
            x,
            MeshBufferVertexAttributeInfo::Visibility(
                MeshBufferVisibilityVertexAttributeInfo::Tangents { .. }
            )
        )
    });

    if has_tangents {
        return Ok(attribute_data);
    }

    // Check if this primitive needs tangents (has a normal map)
    let needs_tangents = primitive.material().normal_texture().is_some() || {
        #[cfg(feature = "clearcoat")]
        {
            primitive
                .material()
                .clearcoat()
                .is_some_and(|cc| cc.clearcoat_normal_texture().is_some())
        }
        #[cfg(not(feature = "clearcoat"))]
        false
    };

    if !needs_tangents {
        return Ok(attribute_data);
    }

    // Check if we have the required data for tangent generation
    let positions = attribute_data.iter().find_map(|(k, v)| match k {
        MeshBufferVertexAttributeInfo::Visibility(
            MeshBufferVisibilityVertexAttributeInfo::Positions { .. },
        ) => Some(v.as_ref()),
        _ => None,
    });

    let normals = attribute_data.iter().find_map(|(k, v)| match k {
        MeshBufferVertexAttributeInfo::Visibility(
            MeshBufferVisibilityVertexAttributeInfo::Normals { .. },
        ) => Some(v.as_ref()),
        _ => None,
    });

    // Find TEXCOORD_0 (UV set 0) - required for tangent calculation
    let texcoords = attribute_data.iter().find_map(|(k, v)| match k {
        MeshBufferVertexAttributeInfo::Custom(MeshBufferCustomVertexAttributeInfo::TexCoords {
            index: 0,
            ..
        }) => Some(v.as_ref()),
        _ => None,
    });

    let (positions, normals, texcoords) = match (positions, normals, texcoords) {
        (Some(p), Some(n), Some(t)) => (p, n, t),
        _ => {
            tracing::warn!(
                "Cannot generate tangents: missing positions, normals, or UV coordinates"
            );
            return Ok(attribute_data);
        }
    };

    // Generate tangents
    let tangents_bytes = compute_tangents(positions, normals, texcoords, triangle_indices)?;

    attribute_data.insert(
        MeshBufferVertexAttributeInfo::Visibility(
            MeshBufferVisibilityVertexAttributeInfo::Tangents {
                data_size: 4,     // f32
                component_len: 4, // vec4 (xyz + handedness w)
            },
        ),
        Cow::Owned(tangents_bytes),
    );

    Ok(attribute_data)
}

/// Wrapper struct for mikktspace Geometry trait implementation
struct MikkTSpaceGeometry<'a> {
    positions: &'a [u8],
    normals: &'a [u8],
    texcoords: &'a [u8],
    triangles: &'a [[usize; 3]],
    tangent_sum: Vec<[f32; 3]>,
    tangent_sign_sum: Vec<f32>,
    tangent_sign_positive_count: Vec<u32>,
    tangent_sign_negative_count: Vec<u32>,
    tangent_count: Vec<u32>,
}

impl<'a> MikkTSpaceGeometry<'a> {
    fn new(
        positions: &'a [u8],
        normals: &'a [u8],
        texcoords: &'a [u8],
        triangles: &'a [[usize; 3]],
        vertex_count: usize,
    ) -> Self {
        Self {
            positions,
            normals,
            texcoords,
            triangles,
            tangent_sum: vec![[0.0, 0.0, 0.0]; vertex_count],
            tangent_sign_sum: vec![0.0; vertex_count],
            tangent_sign_positive_count: vec![0; vertex_count],
            tangent_sign_negative_count: vec![0; vertex_count],
            tangent_count: vec![0; vertex_count],
        }
    }

    fn get_position(&self, vertex_index: usize) -> [f32; 3] {
        let offset = vertex_index * 12; // 3 f32s = 12 bytes
        if offset + 12 > self.positions.len() {
            return [0.0, 0.0, 0.0];
        }
        [
            f32::from_le_bytes(self.positions[offset..offset + 4].try_into().unwrap()),
            f32::from_le_bytes(self.positions[offset + 4..offset + 8].try_into().unwrap()),
            f32::from_le_bytes(self.positions[offset + 8..offset + 12].try_into().unwrap()),
        ]
    }

    fn get_normal(&self, vertex_index: usize) -> [f32; 3] {
        let offset = vertex_index * 12; // 3 f32s = 12 bytes
        if offset + 12 > self.normals.len() {
            return [0.0, 1.0, 0.0];
        }
        [
            f32::from_le_bytes(self.normals[offset..offset + 4].try_into().unwrap()),
            f32::from_le_bytes(self.normals[offset + 4..offset + 8].try_into().unwrap()),
            f32::from_le_bytes(self.normals[offset + 8..offset + 12].try_into().unwrap()),
        ]
    }

    fn get_texcoord(&self, vertex_index: usize) -> [f32; 2] {
        let offset = vertex_index * 8; // 2 f32s = 8 bytes
        if offset + 8 > self.texcoords.len() {
            return [0.0, 0.0];
        }
        [
            f32::from_le_bytes(self.texcoords[offset..offset + 4].try_into().unwrap()),
            f32::from_le_bytes(self.texcoords[offset + 4..offset + 8].try_into().unwrap()),
        ]
    }

    fn finalize_tangents(&self) -> Vec<[f32; 4]> {
        let mut out = Vec::with_capacity(self.tangent_sum.len());

        for vertex_index in 0..self.tangent_sum.len() {
            let count = self.tangent_count[vertex_index];
            if count == 0 {
                out.push([1.0, 0.0, 0.0, 1.0]);
                continue;
            }

            let sum = self.tangent_sum[vertex_index];
            let mut tangent = normalize_or_fallback(sum, self.get_normal(vertex_index));
            // Ensure finite output in all cases.
            if !tangent.iter().all(|v| v.is_finite()) {
                tangent = [1.0, 0.0, 0.0];
            }

            let sign_sum = self.tangent_sign_sum[vertex_index];
            // UV seams can produce nearly-canceling signed tangents for the same shared vertex.
            // Use sign_sum when stable; otherwise fall back to majority vote by sign count.
            const SIGN_EPSILON: f32 = 1e-4;
            let sign = if !sign_sum.is_finite() {
                1.0
            } else if sign_sum.abs() >= SIGN_EPSILON {
                if sign_sum > 0.0 {
                    1.0
                } else {
                    -1.0
                }
            } else if self.tangent_sign_positive_count[vertex_index]
                >= self.tangent_sign_negative_count[vertex_index]
            {
                1.0
            } else {
                -1.0
            };

            out.push([tangent[0], tangent[1], tangent[2], sign]);
        }

        out
    }
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len_sq = dot3(v, v);
    if len_sq > 1e-20 {
        let inv_len = len_sq.sqrt().recip();
        [v[0] * inv_len, v[1] * inv_len, v[2] * inv_len]
    } else {
        [0.0, 0.0, 0.0]
    }
}

fn canonical_tangent_from_normal(normal: [f32; 3]) -> [f32; 3] {
    let n = normalize3(normal);
    let axis = if n[1].abs() < 0.999 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let t = cross3(axis, n);
    let t_norm = normalize3(t);
    if dot3(t_norm, t_norm) > 0.0 {
        t_norm
    } else {
        [1.0, 0.0, 0.0]
    }
}

fn normalize_or_fallback(v: [f32; 3], normal: [f32; 3]) -> [f32; 3] {
    let n = normalize3(normal);
    // Remove any component along the normal before normalization.
    let v_ortho = {
        let proj = dot3(v, n);
        [v[0] - n[0] * proj, v[1] - n[1] * proj, v[2] - n[2] * proj]
    };
    let t = normalize3(v_ortho);
    if dot3(t, t) > 0.0 {
        t
    } else {
        canonical_tangent_from_normal(normal)
    }
}

impl bevy_mikktspace::Geometry for MikkTSpaceGeometry<'_> {
    fn num_faces(&self) -> usize {
        self.triangles.len()
    }

    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3 // Always triangles
    }

    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        let vertex_index = self.triangles[face][vert];
        self.get_position(vertex_index)
    }

    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        let vertex_index = self.triangles[face][vert];
        self.get_normal(vertex_index)
    }

    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        let vertex_index = self.triangles[face][vert];
        self.get_texcoord(vertex_index)
    }

    fn set_tangent_encoded(&mut self, tangent: [f32; 4], face: usize, vert: usize) {
        let vertex_index = self.triangles[face][vert];
        // MikkTSpace can emit different tangents for a shared vertex when UV charts meet.
        // Accumulate and normalize for deterministic per-vertex tangents instead of
        // last-write-wins artifacts.
        self.tangent_sum[vertex_index][0] += tangent[0];
        self.tangent_sum[vertex_index][1] += tangent[1];
        self.tangent_sum[vertex_index][2] += tangent[2];
        self.tangent_sign_sum[vertex_index] += tangent[3];
        if tangent[3] > 0.0 {
            self.tangent_sign_positive_count[vertex_index] += 1;
        } else if tangent[3] < 0.0 {
            self.tangent_sign_negative_count[vertex_index] += 1;
        }
        self.tangent_count[vertex_index] += 1;
    }
}

fn compute_tangents(
    positions: &[u8],
    normals: &[u8],
    texcoords: &[u8],
    triangle_indices: &[[usize; 3]],
) -> Result<Vec<u8>> {
    // Validate buffer sizes
    if positions.len() % 12 != 0 {
        return Err(AwsmGltfError::GenerateTangents(
            "Position buffer length is not a multiple of 12".to_string(),
        ));
    }
    if normals.len() % 12 != 0 {
        return Err(AwsmGltfError::GenerateTangents(
            "Normal buffer length is not a multiple of 12".to_string(),
        ));
    }
    if texcoords.len() % 8 != 0 {
        return Err(AwsmGltfError::GenerateTangents(
            "TexCoord buffer length is not a multiple of 8".to_string(),
        ));
    }

    let vertex_count = positions.len() / 12;

    if triangle_indices.is_empty() {
        return Ok(Vec::new());
    }

    // Create geometry wrapper and generate tangents
    let mut geometry = MikkTSpaceGeometry::new(
        positions,
        normals,
        texcoords,
        triangle_indices,
        vertex_count,
    );

    if !bevy_mikktspace::generate_tangents(&mut geometry) {
        return Err(AwsmGltfError::GenerateTangents(
            "MikkTSpace tangent generation failed".to_string(),
        ));
    }

    // Convert tangents to bytes
    let mut tangents_bytes = Vec::with_capacity(vertex_count * 16); // 4 f32s per tangent
    let final_tangents = geometry.finalize_tangents();
    for tangent in &final_tangents {
        tangents_bytes.extend_from_slice(&tangent[0].to_le_bytes());
        tangents_bytes.extend_from_slice(&tangent[1].to_le_bytes());
        tangents_bytes.extend_from_slice(&tangent[2].to_le_bytes());
        tangents_bytes.extend_from_slice(&tangent[3].to_le_bytes());
    }

    Ok(tangents_bytes)
}
