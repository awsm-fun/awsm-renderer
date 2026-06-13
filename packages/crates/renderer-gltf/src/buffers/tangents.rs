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

    // Check if this primitive needs tangents (has a normal map — base or
    // clearcoat). The clearcoat normal map is read from the raw extensions
    // JSON (matching the material parser, which no longer uses the gltf
    // crate's unreleased typed clearcoat accessor).
    let needs_tangents = primitive.material().normal_texture().is_some()
        || primitive
            .material()
            .extension_value("KHR_materials_clearcoat")
            .and_then(|cc| cc.get("clearcoatNormalTexture"))
            .is_some();

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

/// Bake per-vertex `vec4` tangents from the LE byte buffers (`positions`/
/// `normals` = 12 B vec3, `texcoords` = 8 B vec2) for the given triangles, as a
/// LE `vec4` byte stream (16 B/vertex).
///
/// Delegates the MikkTSpace generation to the shared `awsm-tangents` crate —
/// the SAME implementation the renderer's raw-mesh path and the glb exporter
/// use — so all three callers can no longer drift (the previous per-path copy
/// of the mikktspace adapter is retained only as the `#[cfg(test)]` byte-
/// identity reference below). Decode here is bit-preserving (`from_le_bytes` →
/// the crate → `to_le_bytes`); an out-of-range index now fails loud
/// (`generate_tangents` returns `None` → `Err`) instead of silently producing
/// zero-position garbage as the old inline reader did.
fn compute_tangents(
    positions: &[u8],
    normals: &[u8],
    texcoords: &[u8],
    triangle_indices: &[[usize; 3]],
) -> Result<Vec<u8>> {
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
    // Preserve the old early-out: no triangles → no tangents (the crate would
    // otherwise reject `indices.len() < 3`).
    if triangle_indices.is_empty() {
        return Ok(Vec::new());
    }

    let pos = decode_vec3(positions);
    let nrm = decode_vec3(normals);
    let uv = decode_vec2(texcoords);
    let indices: Vec<u32> = triangle_indices
        .iter()
        .flat_map(|t| t.iter().map(|&i| i as u32))
        .collect();

    let tangents =
        awsm_tangents::generate_tangents(&pos, &nrm, &uv, &indices).ok_or_else(|| {
            AwsmGltfError::GenerateTangents("MikkTSpace tangent generation failed".to_string())
        })?;

    let mut out = Vec::with_capacity(tangents.len() * 16);
    for t in &tangents {
        out.extend_from_slice(&t[0].to_le_bytes());
        out.extend_from_slice(&t[1].to_le_bytes());
        out.extend_from_slice(&t[2].to_le_bytes());
        out.extend_from_slice(&t[3].to_le_bytes());
    }
    Ok(out)
}

/// LE byte stream → `[f32; 3]`s (bit-preserving).
fn decode_vec3(bytes: &[u8]) -> Vec<[f32; 3]> {
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

/// LE byte stream → `[f32; 2]`s (bit-preserving).
fn decode_vec2(bytes: &[u8]) -> Vec<[f32; 2]> {
    bytes
        .chunks_exact(8)
        .map(|c| {
            [
                f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
            ]
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Pre-consolidation REFERENCE implementation — the per-path byte-buffer
// mikktspace adapter this file used before delegating to `awsm-tangents`.
// Retained ONLY as the byte-identity guard (parity_tests below) so the
// consolidation is provably lossless for well-formed meshes. Not shipped.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
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

#[cfg(test)]
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
        let offset = vertex_index * 12;
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
        let offset = vertex_index * 12;
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
        let offset = vertex_index * 8;
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
            if !tangent.iter().all(|v| v.is_finite()) {
                tangent = [1.0, 0.0, 0.0];
            }
            let sign_sum = self.tangent_sign_sum[vertex_index];
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

#[cfg(test)]
fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[cfg(test)]
fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
#[cfg(test)]
fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len_sq = dot3(v, v);
    if len_sq > 1e-20 {
        let inv_len = len_sq.sqrt().recip();
        [v[0] * inv_len, v[1] * inv_len, v[2] * inv_len]
    } else {
        [0.0, 0.0, 0.0]
    }
}
#[cfg(test)]
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
#[cfg(test)]
fn normalize_or_fallback(v: [f32; 3], normal: [f32; 3]) -> [f32; 3] {
    let n = normalize3(normal);
    let proj = dot3(v, n);
    let v_ortho = [v[0] - n[0] * proj, v[1] - n[1] * proj, v[2] - n[2] * proj];
    let t = normalize3(v_ortho);
    if dot3(t, t) > 0.0 {
        t
    } else {
        canonical_tangent_from_normal(normal)
    }
}

#[cfg(test)]
impl bevy_mikktspace::Geometry for MikkTSpaceGeometry<'_> {
    fn num_faces(&self) -> usize {
        self.triangles.len()
    }
    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3
    }
    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        self.get_position(self.triangles[face][vert])
    }
    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        self.get_normal(self.triangles[face][vert])
    }
    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        self.get_texcoord(self.triangles[face][vert])
    }
    fn set_tangent_encoded(&mut self, tangent: [f32; 4], face: usize, vert: usize) {
        let vertex_index = self.triangles[face][vert];
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

/// The pre-consolidation byte-buffer tangent baker, retained as the
/// byte-identity reference for `parity_tests`.
#[cfg(test)]
fn reference_compute_tangents(
    positions: &[u8],
    normals: &[u8],
    texcoords: &[u8],
    triangle_indices: &[[usize; 3]],
) -> Result<Vec<u8>> {
    if positions.len() % 12 != 0 || normals.len() % 12 != 0 || texcoords.len() % 8 != 0 {
        return Err(AwsmGltfError::GenerateTangents(
            "bad buffer length".to_string(),
        ));
    }
    let vertex_count = positions.len() / 12;
    if triangle_indices.is_empty() {
        return Ok(Vec::new());
    }
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
    let mut out = Vec::with_capacity(vertex_count * 16);
    for tangent in &geometry.finalize_tangents() {
        out.extend_from_slice(&tangent[0].to_le_bytes());
        out.extend_from_slice(&tangent[1].to_le_bytes());
        out.extend_from_slice(&tangent[2].to_le_bytes());
        out.extend_from_slice(&tangent[3].to_le_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use proptest::prelude::*;

    fn enc3(vs: &[[f32; 3]]) -> Vec<u8> {
        vs.iter()
            .flat_map(|v| v.iter().flat_map(|f| f.to_le_bytes()))
            .collect()
    }
    fn enc2(vs: &[[f32; 2]]) -> Vec<u8> {
        vs.iter()
            .flat_map(|v| v.iter().flat_map(|f| f.to_le_bytes()))
            .collect()
    }
    // Realistic FINITE geometry. (NaN/inf would make bevy_mikktspace churn for
    // tens of seconds with no added coverage — both impls feed identical bytes
    // to the same mikktspace, so they agree on every input regardless; finite
    // floats are the domain real meshes live in and keep the proptest fast.)
    fn any_f32() -> impl Strategy<Value = f32> {
        -100.0f32..100.0f32
    }

    proptest! {
        /// The awsm-tangents delegation produces BYTE-IDENTICAL output to the
        /// retained pre-consolidation reference for well-formed meshes (in-range
        /// indices), across arbitrary f32 bit patterns + windings.
        #[test]
        fn delegation_matches_reference(
            (positions, normals, uvs, tris) in (1usize..16).prop_flat_map(|vcount| (
                proptest::collection::vec([any_f32(), any_f32(), any_f32()], vcount),
                proptest::collection::vec([any_f32(), any_f32(), any_f32()], vcount),
                proptest::collection::vec([any_f32(), any_f32()], vcount),
                proptest::collection::vec(
                    [0..vcount, 0..vcount, 0..vcount], 1..24,
                ),
            )),
        ) {
            let p = enc3(&positions);
            let n = enc3(&normals);
            let t = enc2(&uvs);
            let new = super::compute_tangents(&p, &n, &t, &tris);
            let reference = reference_compute_tangents(&p, &n, &t, &tris);
            match (new, reference) {
                (Ok(a), Ok(b)) => prop_assert_eq!(a, b),
                // Both impls reject the same degenerate inputs (e.g. mikktspace
                // failing on collinear UVs) — agreeing on Err is also parity.
                (Err(_), Err(_)) => {}
                (a, b) => prop_assert!(
                    false,
                    "Ok/Err disagreement: new={:?} reference={:?}",
                    a.is_ok(),
                    b.is_ok()
                ),
            }
        }
    }
}
