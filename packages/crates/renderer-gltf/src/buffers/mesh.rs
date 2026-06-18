// `transparency` is now entirely a `#[cfg(test)]` packer-parity reference (its
// `create_transparency_vertices` is no longer on the live decode path — the
// renderer packs transparency bytes at commit). `visibility` keeps the live
// source decoders (`resolve_attribute_buffers` / `decode_vec3s`/`vec4s`) plus its
// own `#[cfg(test)]` packer-parity test.
#[cfg(test)]
mod transparency;
mod visibility;

use awsm_renderer_core::pipeline::primitive::FrontFace;

use crate::buffers::attributes::{load_attribute_data_by_kind, pack_vertex_attributes};
use crate::buffers::index::extract_triangle_indices;
use crate::buffers::morph::convert_morph_targets;
use crate::buffers::normals::ensure_normals;
use crate::buffers::skin::convert_skin;
use crate::buffers::triangle::pack_triangle_data;
use crate::buffers::{
    MeshBufferAttributeIndexInfoWithOffset, MeshBufferInfoWithOffset,
    MeshBufferTriangleInfoWithOffset,
};
#[cfg(test)]
use crate::error::AwsmGltfError;
use awsm_renderer::meshes::buffer_info::MeshBufferVertexAttributeInfo;

use super::Result;

// The geometry KIND (visibility vs transparency) is no longer decided in the
// decode — the renderer derives it at `commit_load` from the union of materials
// bound to each geometry, via the single `geometry_kind` fn (docs/plans/todo.md §4).
// The decode now retains the pass-INDEPENDENT typed source (positions/normals/uvs0/
// authored-tangents/indices) + custom attributes + morph/skin; the per-pass byte
// streams are packed at commit. `create_visibility_vertices` /
// `create_transparency_vertices` survive only as `#[cfg(test)]` packer-parity tests.

#[allow(clippy::too_many_arguments)]
pub(super) fn convert_to_mesh_buffer(
    primitive: &gltf::Primitive,
    render_timings: bool,
    front_face: FrontFace,
    buffers: &[Vec<u8>],
    custom_attribute_index: &MeshBufferAttributeIndexInfoWithOffset,
    custom_attribute_index_bytes: &[u8],
    custom_attribute_vertex_bytes: &mut Vec<u8>,
    triangle_data_bytes: &mut Vec<u8>,
    geometry_morph_bytes: &mut Vec<u8>,
    material_morph_bytes: &mut Vec<u8>,
    skin_joint_index_weight_bytes: &mut Vec<u8>,
) -> Result<MeshBufferInfoWithOffset> {
    let _maybe_primitive_span_guard = if render_timings {
        Some(
            tracing::span!(
                tracing::Level::INFO,
                "GLTF primitive buffer convert",
                primitive_index = primitive.index(),
                material_index = primitive.material().index()
            )
            .entered(),
        )
    } else {
        None
    };

    // Step 1: Load all GLTF attributes
    let gltf_attributes: Vec<(gltf::Semantic, gltf::Accessor<'_>)> = {
        let _maybe_stage_span_guard = if render_timings {
            Some(tracing::span!(tracing::Level::INFO, "collect_attributes").entered())
        } else {
            None
        };
        primitive
            .attributes()
            .filter(|(semantic, _)| {
                // Joints and Weights are NOT vertex attributes - they're skinning data
                // Handled separately by convert_skin(), never enter the attribute system
                !matches!(
                    semantic,
                    gltf::Semantic::Joints(_) | gltf::Semantic::Weights(_)
                )
            })
            .collect()
    };

    // this should never be empty, but let's be safe
    let vertex_count = gltf_attributes
        .first()
        .map(|(_, accessor)| accessor.count())
        .unwrap_or(0);

    let triangle_count = custom_attribute_index.count / 3;
    let triangle_indices = {
        let _maybe_stage_span_guard = if render_timings {
            Some(tracing::span!(tracing::Level::INFO, "extract_triangle_indices").entered())
        } else {
            None
        };
        extract_triangle_indices(custom_attribute_index, custom_attribute_index_bytes)?
    };

    // Step 2: Load attribute data by kind
    let attribute_data_by_kind = {
        let _maybe_stage_span_guard = if render_timings {
            Some(tracing::span!(tracing::Level::INFO, "load_attribute_data_by_kind").entered())
        } else {
            None
        };
        load_attribute_data_by_kind(&gltf_attributes, buffers)?
    };

    // Step 3: Ensure normals exist (compute if missing)
    let attribute_data_by_kind = {
        let _maybe_stage_span_guard = if render_timings {
            Some(tracing::span!(tracing::Level::INFO, "ensure_normals").entered())
        } else {
            None
        };
        ensure_normals(attribute_data_by_kind, &triangle_indices)?
    };

    // Step 3a (§5b): retain the TYPED source for the renderer's GeometrySource.
    // `source_tangents` carries only AUTHORED tangents (a glTF `TANGENT` attribute);
    // generation is deferred to commit, gated on the bound material (so meshes that
    // don't sample a normal map pay nothing). positions/normals are guaranteed
    // present after ensure_normals.
    let (source_positions, source_normals, source_tangents, source_uvs0) = {
        use crate::buffers::mesh::visibility::{
            decode_vec3s, decode_vec4s, resolve_attribute_buffers,
        };
        let (pos_b, norm_b, tan_b) = resolve_attribute_buffers(&attribute_data_by_kind)?;
        let uvs0 = attribute_data_by_kind.iter().find_map(|(attr, data)| {
            match attr {
            MeshBufferVertexAttributeInfo::Custom(
                awsm_renderer::meshes::buffer_info::MeshBufferCustomVertexAttributeInfo::TexCoords {
                    index: 0,
                    ..
                },
            ) => Some(
                data.chunks_exact(8)
                    .map(|c| {
                        [
                            f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                            f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                        ]
                    })
                    .collect::<Vec<[f32; 2]>>(),
            ),
            _ => None,
        }
        });
        (
            decode_vec3s(pos_b),
            decode_vec3s(norm_b),
            tan_b.map(decode_vec4s),
            uvs0,
        )
    };
    let source_indices: Vec<u32> = triangle_indices
        .iter()
        .flatten()
        .map(|&i| i as u32)
        .collect();

    // (The per-pass visibility/transparency vertex streams + tangent generation are
    // no longer built here — the renderer packs them at commit from the retained
    // source above, per the union of bound materials. See module note.)

    // Step 5: Pack vertex attributes
    // These are the original attributes per-vertex, but only non-visibility ones
    // There is no need to repack or expand these, they are used as-is
    let attribute_vertex_offset = {
        let _maybe_stage_span_guard = if render_timings {
            Some(tracing::span!(tracing::Level::INFO, "pack_vertex_attributes").entered())
        } else {
            None
        };
        let attribute_vertex_offset = custom_attribute_vertex_bytes.len();
        pack_vertex_attributes(
            attribute_data_by_kind
                .iter()
                .filter_map(|x| match x.0 {
                    MeshBufferVertexAttributeInfo::Custom(custom) => Some((custom, x.1)),
                    _ => None,
                })
                .collect(),
            custom_attribute_vertex_bytes,
        )?;
        attribute_vertex_offset
    };

    // Step 6: Pack triangle data (vertex indices)
    let triangle_data_offset = triangle_data_bytes.len();
    let triangle_data_info = {
        let _maybe_stage_span_guard = if render_timings {
            Some(tracing::span!(tracing::Level::INFO, "pack_triangle_data").entered())
        } else {
            None
        };
        pack_triangle_data(
            &triangle_indices,
            triangle_count,
            triangle_data_offset,
            triangle_data_bytes,
            front_face,
            primitive.material().double_sided(),
        )?
    };

    // Step 7: Handle morph targets (if any)
    let (geometry_morph, material_morph) = {
        let _maybe_stage_span_guard = if render_timings {
            Some(tracing::span!(tracing::Level::INFO, "convert_morph_targets").entered())
        } else {
            None
        };
        convert_morph_targets(
            primitive,
            buffers,
            vertex_count,
            geometry_morph_bytes,
            material_morph_bytes,
        )?
    };

    // Step 8: Handle skin (if any)
    let skin = {
        let _maybe_stage_span_guard = if render_timings {
            Some(tracing::span!(tracing::Level::INFO, "convert_skin").entered())
        } else {
            None
        };
        convert_skin(
            primitive,
            buffers,
            vertex_count,
            skin_joint_index_weight_bytes,
        )?
    };

    // Step 7: Build final MeshBufferInfo
    Ok(MeshBufferInfoWithOffset {
        triangles: MeshBufferTriangleInfoWithOffset {
            count: triangle_count,
            vertex_attribute_indices: custom_attribute_index.clone(),
            vertex_attributes: attribute_data_by_kind
                .keys()
                .filter(|attr| attr.is_custom_attribute())
                .cloned()
                .collect(),
            vertex_attributes_offset: attribute_vertex_offset,
            vertex_attributes_size: custom_attribute_vertex_bytes.len() - attribute_vertex_offset,
            triangle_data: triangle_data_info,
        },
        geometry_morph,
        material_morph,
        skin,
        source_positions,
        source_normals,
        source_uvs0,
        source_tangents,
        source_indices,
        source_front_face: front_face,
    })
}

// Used only by the Phase-2b byte-identity REFERENCE writers (cfg(test)).
#[cfg(test)]
fn get_position_from_buffer(positions: &[u8], vertex_index: usize) -> Result<[f32; 3]> {
    let offset = vertex_index * 12; // 3 f32s = 12 bytes

    let vertex_count = positions.len() / 12;
    if vertex_index >= vertex_count {
        return Err(AwsmGltfError::Positions(format!(
            "Position data out of bounds for vertex {}. Buffer has {} vertices ({} bytes), requested vertex {}",
            vertex_index, vertex_count, positions.len(), vertex_index
        )));
    }

    if offset + 12 > positions.len() {
        return Err(AwsmGltfError::Positions(format!(
            "Position data out of bounds for vertex {}. Offset {} + 12 > buffer size {}",
            vertex_index,
            offset,
            positions.len()
        )));
    }

    // From spec:
    // "All buffer data defined in this specification (i.e., geometry attributes, geometry indices, sparse accessor data, animation inputs and outputs, inverse bind matrices)
    // MUST use little endian byte order."
    let x = f32::from_le_bytes([
        positions[offset],
        positions[offset + 1],
        positions[offset + 2],
        positions[offset + 3],
    ]);
    let y = f32::from_le_bytes([
        positions[offset + 4],
        positions[offset + 5],
        positions[offset + 6],
        positions[offset + 7],
    ]);
    let z = f32::from_le_bytes([
        positions[offset + 8],
        positions[offset + 9],
        positions[offset + 10],
        positions[offset + 11],
    ]);

    Ok([x, y, z])
}

// Used only by the Phase-2b byte-identity REFERENCE writers (cfg(test)).
#[cfg(test)]
fn get_vec3_from_buffer(buffer: &[u8], vertex_index: usize, name: &str) -> Result<[f32; 3]> {
    let offset = vertex_index * 12; // 3 f32s = 12 bytes

    let vertex_count = buffer.len() / 12;
    if vertex_index >= vertex_count {
        return Err(AwsmGltfError::AttributeData(format!(
            "{} data out of bounds for vertex {}. Buffer has {} vertices ({} bytes), requested vertex {}",
            name, vertex_index, vertex_count, buffer.len(), vertex_index
        )));
    }

    if offset + 12 > buffer.len() {
        return Err(AwsmGltfError::AttributeData(format!(
            "{} data out of bounds for vertex {}. Offset {} + 12 > buffer size {}",
            name,
            vertex_index,
            offset,
            buffer.len()
        )));
    }

    let x = f32::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ]);
    let y = f32::from_le_bytes([
        buffer[offset + 4],
        buffer[offset + 5],
        buffer[offset + 6],
        buffer[offset + 7],
    ]);
    let z = f32::from_le_bytes([
        buffer[offset + 8],
        buffer[offset + 9],
        buffer[offset + 10],
        buffer[offset + 11],
    ]);

    Ok([x, y, z])
}

// Used only by the Phase-2b byte-identity REFERENCE writers (cfg(test)).
#[cfg(test)]
fn get_vec4_from_buffer(buffer: &[u8], vertex_index: usize, name: &str) -> Result<[f32; 4]> {
    let offset = vertex_index * 16; // 4 f32s = 16 bytes

    let vertex_count = buffer.len() / 16;
    if vertex_index >= vertex_count {
        return Err(AwsmGltfError::AttributeData(format!(
            "{} data out of bounds for vertex {}. Buffer has {} vertices ({} bytes), requested vertex {}",
            name, vertex_index, vertex_count, buffer.len(), vertex_index
        )));
    }

    if offset + 16 > buffer.len() {
        return Err(AwsmGltfError::AttributeData(format!(
            "{} data out of bounds for vertex {}. Offset {} + 16 > buffer size {}",
            name,
            vertex_index,
            offset,
            buffer.len()
        )));
    }

    let x = f32::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ]);
    let y = f32::from_le_bytes([
        buffer[offset + 4],
        buffer[offset + 5],
        buffer[offset + 6],
        buffer[offset + 7],
    ]);
    let z = f32::from_le_bytes([
        buffer[offset + 8],
        buffer[offset + 9],
        buffer[offset + 10],
        buffer[offset + 11],
    ]);
    let w = f32::from_le_bytes([
        buffer[offset + 12],
        buffer[offset + 13],
        buffer[offset + 14],
        buffer[offset + 15],
    ]);

    Ok([x, y, z, w])
}
