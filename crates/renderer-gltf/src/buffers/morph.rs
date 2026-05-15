use std::borrow::Cow;

use crate::buffers::accessor::accessor_to_bytes;
use crate::buffers::{
    MeshBufferGeometryMorphInfoWithOffset, MeshBufferMaterialMorphInfoWithOffset,
};
use crate::error::{AwsmGltfError, Result};
use awsm_renderer::buffer::helpers::slice_zeroes;

/// Converts GLTF morph targets into storage buffer with indexed per-vertex format
///
/// IMPORTANT: Morphing data is NOT stored as vertex attributes.
/// It is stored in dedicated morph storage buffers and accessed by the geometry pass.
/// This separation ensures:
/// - Memory efficiency (no duplication)
/// - Clear architecture (morphs ≠ attributes)
/// - Type safety (custom meshes can't accidentally add morph data as attributes)
///
///
/// KEY CONCEPT - Indexed Format:
///
/// ALL MORPHS (Position + Normal + Tangent):
/// - Stored per original vertex (not exploded)
/// - Layout per vertex: [Target0[pos, norm, tang], Target1[pos, norm, tang], ...]
/// - Shaders access via original vertex index from vertex buffer
///
/// Data is interleaved per vertex:
/// Vertex0: [T0_pos(3), T0_norm(3), T0_tang(4), T1_pos(3), T1_norm(3), T1_tang(4), ...]
/// Vertex1: [T0_pos(3), T0_norm(3), T0_tang(4), T1_pos(3), T1_norm(3), T1_tang(4), ...]
/// Vertex2: [T0_pos(3), T0_norm(3), T0_tang(4), T1_pos(3), T1_norm(3), T1_tang(4), ...]
pub(super) fn convert_morph_targets(
    primitive: &gltf::Primitive,
    buffers: &[Vec<u8>],
    vertex_count: usize,
    geometry_morph_bytes: &mut Vec<u8>, // Indexed position + normal + tangent morphs
    _material_morph_bytes: &mut Vec<u8>, // Could be used for other material morphs, theoretically
) -> Result<(
    Option<MeshBufferGeometryMorphInfoWithOffset>,
    Option<MeshBufferMaterialMorphInfoWithOffset>,
)> {
    let has_any_position_morph = primitive
        .morph_targets()
        .any(|morph_target| morph_target.positions().is_some());
    let has_any_normal_morph = primitive
        .morph_targets()
        .any(|morph_target| morph_target.normals().is_some());

    let has_any_tangent_morph = primitive
        .morph_targets()
        .any(|morph_target| morph_target.tangents().is_some());

    if !has_any_position_morph && !has_any_normal_morph && !has_any_tangent_morph {
        return Ok((None, None));
    }

    // Load all morph target data from GLTF
    // This is the ORIGINAL per-vertex morph data (deltas from base mesh)
    #[derive(Default)]
    struct MorphTargetBufferData<'a> {
        positions: Option<Cow<'a, [u8]>>, // Position deltas (vec3<f32> per original vertex)
        normals: Option<Cow<'a, [u8]>>,   // Normal deltas (vec3<f32> per original vertex)
        tangents: Option<Cow<'a, [u8]>>,  // Tangent deltas (vec3<f32> per original vertex, no W)
    }

    let mut morph_targets_buffer_data = Vec::new();
    for morph_target in primitive.morph_targets() {
        let mut morph_target_buffer_data = MorphTargetBufferData::default();

        if let Some(accessor) = morph_target.positions() {
            morph_target_buffer_data.positions = Some(accessor_to_bytes(&accessor, buffers)?);
        }
        if let Some(accessor) = morph_target.normals() {
            morph_target_buffer_data.normals = Some(accessor_to_bytes(&accessor, buffers)?);
        }
        if let Some(accessor) = morph_target.tangents() {
            morph_target_buffer_data.tangents = Some(accessor_to_bytes(&accessor, buffers)?);
        }

        morph_targets_buffer_data.push(morph_target_buffer_data);
    }

    let targets_len = morph_targets_buffer_data.len();

    // Get original vertex count for material morphs
    let _original_vertex_count = primitive
        .attributes()
        .next()
        .map(|(_, accessor)| accessor.count())
        .unwrap_or(0);

    // INDEXED MORPHS: Store all morph targets (position, normal, tangent) per original vertex
    let geometry_morph_info = if has_any_position_morph
        || has_any_normal_morph
        || has_any_tangent_morph
    {
        let geometry_values_offset = geometry_morph_bytes.len();

        // Size of data for ONE vertex, ONE target:
        // position (3 floats) + normal (3 floats) + tangent (4 floats) = 10 floats = 40 bytes
        let size_per_target_per_vertex = 40;

        // Size of data for ONE vertex, ALL targets
        let vertex_stride_size = size_per_target_per_vertex * targets_len;

        // INDEXED MORPH DATA (per original vertex, not exploded)
        // Store one entry per original vertex
        //
        // INTERLEAVING PATTERN (indexed):
        // Vertex 0: [T0_pos(3), T0_norm(3), T0_tang(4), T1_pos(3), T1_norm(3), T1_tang(4), ...]
        // Vertex 1: [T0_pos(3), T0_norm(3), T0_tang(4), T1_pos(3), T1_norm(3), T1_tang(4), ...]
        // Vertex 2: [T0_pos(3), T0_norm(3), T0_tang(4), T1_pos(3), T1_norm(3), T1_tang(4), ...]
        //   ... etc
        for vertex_index in 0..vertex_count {
            // For each morph target (interleaved per vertex)
            for morph_target_buffer_data in &morph_targets_buffer_data {
                // Position (3 floats = 12 bytes)
                match &morph_target_buffer_data.positions {
                    Some(position_data) => {
                        let data_byte_offset = vertex_index * 12;
                        if data_byte_offset + 12 > position_data.len() {
                            return Err(AwsmGltfError::ConstructNormals(format!(
                                "Position morph data out of bounds for vertex {}",
                                vertex_index
                            )));
                        }
                        let position_bytes =
                            &position_data[data_byte_offset..data_byte_offset + 12];
                        geometry_morph_bytes.extend_from_slice(position_bytes);
                    }
                    None => {
                        geometry_morph_bytes.extend_from_slice(slice_zeroes(12));
                    }
                }

                // Normal (3 floats = 12 bytes)
                match &morph_target_buffer_data.normals {
                    Some(normal_data) => {
                        let data_byte_offset = vertex_index * 12;
                        if data_byte_offset + 12 > normal_data.len() {
                            return Err(AwsmGltfError::ConstructNormals(format!(
                                "Normal morph data out of bounds for vertex {}",
                                vertex_index
                            )));
                        }
                        let normal_bytes = &normal_data[data_byte_offset..data_byte_offset + 12];
                        geometry_morph_bytes.extend_from_slice(normal_bytes);
                    }
                    None => {
                        geometry_morph_bytes.extend_from_slice(slice_zeroes(12));
                    }
                }

                // Tangent (3 floats in GLTF morph, padded to 4 floats = 16 bytes for vec4)
                match &morph_target_buffer_data.tangents {
                    Some(tangent_data) => {
                        let data_byte_offset = vertex_index * 12; // GLTF tangent morphs are vec3
                        if data_byte_offset + 12 > tangent_data.len() {
                            return Err(AwsmGltfError::ConstructNormals(format!(
                                "Tangent morph data out of bounds for vertex {}",
                                vertex_index
                            )));
                        }
                        let tangent_bytes = &tangent_data[data_byte_offset..data_byte_offset + 12];
                        geometry_morph_bytes.extend_from_slice(tangent_bytes);
                        // Pad with 4 zero bytes to make vec4 (w component is 0 for morphs)
                        geometry_morph_bytes.extend_from_slice(&[0u8; 4]);
                    }
                    None => {
                        geometry_morph_bytes.extend_from_slice(slice_zeroes(16));
                    }
                }
            }
        }

        let geometry_values_size = geometry_morph_bytes.len() - geometry_values_offset;

        Some(MeshBufferGeometryMorphInfoWithOffset {
            targets_len,
            vertex_stride_size,
            values_size: geometry_values_size,
            values_offset: geometry_values_offset,
        })
    } else {
        None
    };

    // We don't actually have any material morphs atm
    // Return None for material_morph_info to maintain API compatibility
    Ok((geometry_morph_info, None))
}
