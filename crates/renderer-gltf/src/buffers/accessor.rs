//! glTF accessor decoding helpers.

use std::borrow::Cow;

use awsm_renderer_core::pipeline::vertex::VertexFormat;
use gltf::accessor::{DataType, Dimensions};

use super::Result;
use awsm_renderer::buffer::helpers::{
    u8_to_f32_vec, u8_to_i16_vec, u8_to_i8_vec, u8_to_u16_vec, u8_to_u32_vec,
};

/// Extracts accessor data into a byte slice, handling sparse accessors.
pub fn accessor_to_bytes<'a>(
    accessor: &gltf::Accessor<'_>,
    buffers: &'a [Vec<u8>],
) -> Result<Cow<'a, [u8]>> {
    let mut buffer: Cow<[u8]> = match accessor.view() {
        Some(view) => {
            let buffer = &buffers[view.buffer().index()];
            let buffer = &buffer[accessor.offset() + view.offset()..];

            match view.stride() {
                None => Cow::Borrowed(&buffer[..accessor.size() * accessor.count()]),
                Some(stride) => {
                    let mut repacked = Vec::with_capacity(accessor.size() * accessor.count());
                    for i in 0..accessor.count() {
                        let start = i * stride;
                        repacked.extend_from_slice(&buffer[start..start + accessor.size()])
                    }
                    Cow::Owned(repacked)
                }
            }
        }
        None => {
            // gltf spec says if we have no view, fill it with zeroes
            // and these may or may not be overwritten with sparse bytes (and/or extensions)
            let length = accessor.size() * accessor.count();
            Cow::Owned(vec![0; length])
        }
    };

    if let Some(sparse) = accessor.sparse() {
        // will only clone if borrowed
        let buffer = buffer.to_mut();

        let indices = sparse_to_indices(&sparse, buffers);

        let values_buffer_slice = &buffers[sparse.values().view().buffer().index()];
        let values_buffer_slice_start = sparse.values().offset() + sparse.values().view().offset();
        let values_buffer_slice = &values_buffer_slice[values_buffer_slice_start..];

        for (value_index, target_index) in indices.into_iter().enumerate() {
            let value_slice_start = value_index * accessor.size();
            let value_slice =
                &values_buffer_slice[value_slice_start..value_slice_start + accessor.size()];

            let buffer_slice_start = target_index * accessor.size();
            let buffer_slice =
                &mut buffer[buffer_slice_start..buffer_slice_start + accessor.size()];

            buffer_slice.copy_from_slice(value_slice);
        }
    }

    Ok(buffer)
}

fn sparse_to_indices(
    sparse: &gltf::accessor::sparse::Sparse<'_>,
    buffers: &[Vec<u8>],
) -> Vec<usize> {
    let indices_buffer_slice = &buffers[sparse.indices().view().buffer().index()];
    let indices_buffer_slice_start = sparse.indices().offset() + sparse.indices().view().offset();
    let indices_buffer_slice = &indices_buffer_slice[indices_buffer_slice_start..];

    let mut index_offset = 0;
    let index_offset_amount = sparse.indices().index_type().size();

    let mut indices = Vec::with_capacity(sparse.count());

    for _ in 0..sparse.count() {
        // "All buffer data defined in this specification [...] MUST use little endian byte order."
        // https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html#buffers-and-buffer-views-overview
        let index = match sparse.indices().index_type() {
            gltf::accessor::sparse::IndexType::U8 => {
                let index = indices_buffer_slice[index_offset];
                index as usize
            }
            gltf::accessor::sparse::IndexType::U16 => {
                let index = indices_buffer_slice[index_offset..index_offset + 2]
                    .try_into()
                    .unwrap();
                u16::from_le_bytes(index) as usize
            }
            gltf::accessor::sparse::IndexType::U32 => {
                let index = indices_buffer_slice[index_offset..index_offset + 4]
                    .try_into()
                    .unwrap();
                u32::from_le_bytes(index) as usize
            }
        };
        indices.push(index);
        index_offset += index_offset_amount;
    }

    indices
}

/// Decodes accessor data into a typed vector representation.
pub fn accessor_to_vec(accessor: &gltf::Accessor<'_>, buffers: &[Vec<u8>]) -> Result<AccessorVec> {
    let bytes = accessor_to_bytes(accessor, buffers)?;

    Ok(match accessor.data_type() {
        gltf::accessor::DataType::I8 => {
            let values = u8_to_i8_vec(&bytes);
            match accessor.dimensions() {
                gltf::accessor::Dimensions::Scalar => AccessorVec::ScalarI8(values),
                gltf::accessor::Dimensions::Vec2 => {
                    AccessorVec::Vec2I8(values.chunks_exact(2).map(|v| [v[0], v[1]]).collect())
                }
                gltf::accessor::Dimensions::Vec3 => AccessorVec::Vec3I8(
                    values.chunks_exact(3).map(|v| [v[0], v[1], v[2]]).collect(),
                ),
                gltf::accessor::Dimensions::Vec4 => AccessorVec::Vec4I8(
                    values
                        .chunks_exact(4)
                        .map(|v| [v[0], v[1], v[2], v[3]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat2 => AccessorVec::Mat2I8(
                    values
                        .chunks_exact(4)
                        .map(|v| [[v[0], v[1]], [v[2], v[3]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat3 => AccessorVec::Mat3I8(
                    values
                        .chunks_exact(9)
                        .map(|v| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat4 => AccessorVec::Mat4I8(
                    values
                        .chunks_exact(16)
                        .map(|v| {
                            [
                                [v[0], v[1], v[2], v[3]],
                                [v[4], v[5], v[6], v[7]],
                                [v[8], v[9], v[10], v[11]],
                                [v[12], v[13], v[14], v[15]],
                            ]
                        })
                        .collect(),
                ),
            }
        }
        gltf::accessor::DataType::U8 => {
            let values = bytes;
            match accessor.dimensions() {
                gltf::accessor::Dimensions::Scalar => AccessorVec::ScalarU8(values.to_vec()),
                gltf::accessor::Dimensions::Vec2 => {
                    AccessorVec::Vec2U8(values.chunks_exact(2).map(|v| [v[0], v[1]]).collect())
                }
                gltf::accessor::Dimensions::Vec3 => AccessorVec::Vec3U8(
                    values.chunks_exact(3).map(|v| [v[0], v[1], v[2]]).collect(),
                ),
                gltf::accessor::Dimensions::Vec4 => AccessorVec::Vec4U8(
                    values
                        .chunks_exact(4)
                        .map(|v| [v[0], v[1], v[2], v[3]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat2 => AccessorVec::Mat2U8(
                    values
                        .chunks_exact(4)
                        .map(|v| [[v[0], v[1]], [v[2], v[3]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat3 => AccessorVec::Mat3U8(
                    values
                        .chunks_exact(9)
                        .map(|v| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat4 => AccessorVec::Mat4U8(
                    values
                        .chunks_exact(16)
                        .map(|v| {
                            [
                                [v[0], v[1], v[2], v[3]],
                                [v[4], v[5], v[6], v[7]],
                                [v[8], v[9], v[10], v[11]],
                                [v[12], v[13], v[14], v[15]],
                            ]
                        })
                        .collect(),
                ),
            }
        }
        gltf::accessor::DataType::I16 => {
            let values = u8_to_i16_vec(&bytes);
            match accessor.dimensions() {
                gltf::accessor::Dimensions::Scalar => AccessorVec::ScalarI16(values.to_vec()),
                gltf::accessor::Dimensions::Vec2 => {
                    AccessorVec::Vec2I16(values.chunks_exact(2).map(|v| [v[0], v[1]]).collect())
                }
                gltf::accessor::Dimensions::Vec3 => AccessorVec::Vec3I16(
                    values.chunks_exact(3).map(|v| [v[0], v[1], v[2]]).collect(),
                ),
                gltf::accessor::Dimensions::Vec4 => AccessorVec::Vec4I16(
                    values
                        .chunks_exact(4)
                        .map(|v| [v[0], v[1], v[2], v[3]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat2 => AccessorVec::Mat2I16(
                    values
                        .chunks_exact(4)
                        .map(|v| [[v[0], v[1]], [v[2], v[3]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat3 => AccessorVec::Mat3I16(
                    values
                        .chunks_exact(9)
                        .map(|v| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat4 => AccessorVec::Mat4I16(
                    values
                        .chunks_exact(16)
                        .map(|v| {
                            [
                                [v[0], v[1], v[2], v[3]],
                                [v[4], v[5], v[6], v[7]],
                                [v[8], v[9], v[10], v[11]],
                                [v[12], v[13], v[14], v[15]],
                            ]
                        })
                        .collect(),
                ),
            }
        }
        gltf::accessor::DataType::U16 => {
            let values = u8_to_u16_vec(&bytes);
            match accessor.dimensions() {
                gltf::accessor::Dimensions::Scalar => AccessorVec::ScalarU16(values.to_vec()),
                gltf::accessor::Dimensions::Vec2 => {
                    AccessorVec::Vec2U16(values.chunks_exact(2).map(|v| [v[0], v[1]]).collect())
                }
                gltf::accessor::Dimensions::Vec3 => AccessorVec::Vec3U16(
                    values.chunks_exact(3).map(|v| [v[0], v[1], v[2]]).collect(),
                ),
                gltf::accessor::Dimensions::Vec4 => AccessorVec::Vec4U16(
                    values
                        .chunks_exact(4)
                        .map(|v| [v[0], v[1], v[2], v[3]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat2 => AccessorVec::Mat2U16(
                    values
                        .chunks_exact(4)
                        .map(|v| [[v[0], v[1]], [v[2], v[3]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat3 => AccessorVec::Mat3U16(
                    values
                        .chunks_exact(9)
                        .map(|v| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat4 => AccessorVec::Mat4U16(
                    values
                        .chunks_exact(16)
                        .map(|v| {
                            [
                                [v[0], v[1], v[2], v[3]],
                                [v[4], v[5], v[6], v[7]],
                                [v[8], v[9], v[10], v[11]],
                                [v[12], v[13], v[14], v[15]],
                            ]
                        })
                        .collect(),
                ),
            }
        }
        gltf::accessor::DataType::U32 => {
            let values = u8_to_u32_vec(&bytes);
            match accessor.dimensions() {
                gltf::accessor::Dimensions::Scalar => AccessorVec::ScalarU32(values.to_vec()),
                gltf::accessor::Dimensions::Vec2 => {
                    AccessorVec::Vec2U32(values.chunks_exact(2).map(|v| [v[0], v[1]]).collect())
                }
                gltf::accessor::Dimensions::Vec3 => AccessorVec::Vec3U32(
                    values.chunks_exact(3).map(|v| [v[0], v[1], v[2]]).collect(),
                ),
                gltf::accessor::Dimensions::Vec4 => AccessorVec::Vec4U32(
                    values
                        .chunks_exact(4)
                        .map(|v| [v[0], v[1], v[2], v[3]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat2 => AccessorVec::Mat2U32(
                    values
                        .chunks_exact(4)
                        .map(|v| [[v[0], v[1]], [v[2], v[3]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat3 => AccessorVec::Mat3U32(
                    values
                        .chunks_exact(9)
                        .map(|v| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat4 => AccessorVec::Mat4U32(
                    values
                        .chunks_exact(16)
                        .map(|v| {
                            [
                                [v[0], v[1], v[2], v[3]],
                                [v[4], v[5], v[6], v[7]],
                                [v[8], v[9], v[10], v[11]],
                                [v[12], v[13], v[14], v[15]],
                            ]
                        })
                        .collect(),
                ),
            }
        }
        gltf::accessor::DataType::F32 => {
            let values = u8_to_f32_vec(&bytes);
            match accessor.dimensions() {
                gltf::accessor::Dimensions::Scalar => AccessorVec::ScalarF32(values.to_vec()),
                gltf::accessor::Dimensions::Vec2 => {
                    AccessorVec::Vec2F32(values.chunks_exact(2).map(|v| [v[0], v[1]]).collect())
                }
                gltf::accessor::Dimensions::Vec3 => AccessorVec::Vec3F32(
                    values.chunks_exact(3).map(|v| [v[0], v[1], v[2]]).collect(),
                ),
                gltf::accessor::Dimensions::Vec4 => AccessorVec::Vec4F32(
                    values
                        .chunks_exact(4)
                        .map(|v| [v[0], v[1], v[2], v[3]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat2 => AccessorVec::Mat2F32(
                    values
                        .chunks_exact(4)
                        .map(|v| [[v[0], v[1]], [v[2], v[3]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat3 => AccessorVec::Mat3F32(
                    values
                        .chunks_exact(9)
                        .map(|v| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
                        .collect(),
                ),
                gltf::accessor::Dimensions::Mat4 => AccessorVec::Mat4F32(
                    values
                        .chunks_exact(16)
                        .map(|v| {
                            [
                                [v[0], v[1], v[2], v[3]],
                                [v[4], v[5], v[6], v[7]],
                                [v[8], v[9], v[10], v[11]],
                                [v[12], v[13], v[14], v[15]],
                            ]
                        })
                        .collect(),
                ),
            }
        }
    })
}

/// Typed accessor data decoded from glTF buffers.
#[derive(Debug, Clone, PartialEq)]
pub enum AccessorVec {
    ScalarU8(Vec<u8>),
    ScalarI8(Vec<i8>),
    ScalarU16(Vec<u16>),
    ScalarI16(Vec<i16>),
    ScalarU32(Vec<u32>),
    ScalarF32(Vec<f32>),
    Vec2U8(Vec<[u8; 2]>),
    Vec2I8(Vec<[i8; 2]>),
    Vec2U16(Vec<[u16; 2]>),
    Vec2I16(Vec<[i16; 2]>),
    Vec2U32(Vec<[u32; 2]>),
    Vec2F32(Vec<[f32; 2]>),
    Vec3U8(Vec<[u8; 3]>),
    Vec3I8(Vec<[i8; 3]>),
    Vec3U16(Vec<[u16; 3]>),
    Vec3I16(Vec<[i16; 3]>),
    Vec3U32(Vec<[u32; 3]>),
    Vec3F32(Vec<[f32; 3]>),
    Vec4U8(Vec<[u8; 4]>),
    Vec4I8(Vec<[i8; 4]>),
    Vec4U16(Vec<[u16; 4]>),
    Vec4I16(Vec<[i16; 4]>),
    Vec4U32(Vec<[u32; 4]>),
    Vec4F32(Vec<[f32; 4]>),
    Mat2U8(Vec<[[u8; 2]; 2]>),
    Mat2I8(Vec<[[i8; 2]; 2]>),
    Mat2U16(Vec<[[u16; 2]; 2]>),
    Mat2I16(Vec<[[i16; 2]; 2]>),
    Mat2U32(Vec<[[u32; 2]; 2]>),
    Mat2F32(Vec<[[f32; 2]; 2]>),
    Mat3U8(Vec<[[u8; 3]; 3]>),
    Mat3I8(Vec<[[i8; 3]; 3]>),
    Mat3U16(Vec<[[u16; 3]; 3]>),
    Mat3I16(Vec<[[i16; 3]; 3]>),
    Mat3U32(Vec<[[u32; 3]; 3]>),
    Mat3F32(Vec<[[f32; 3]; 3]>),
    Mat4U8(Vec<[[u8; 4]; 4]>),
    Mat4I8(Vec<[[i8; 4]; 4]>),
    Mat4U16(Vec<[[u16; 4]; 4]>),
    Mat4I16(Vec<[[i16; 4]; 4]>),
    Mat4U32(Vec<[[u32; 4]; 4]>),
    Mat4F32(Vec<[[f32; 4]; 4]>),
}

pub(super) fn _accessor_vertex_format(
    data_type: DataType,
    dimensions: Dimensions,
    normalized: bool,
) -> VertexFormat {
    // https://gpuweb.github.io/gpuweb/#enumdef-gpuvertexformat
    match (data_type, dimensions, normalized) {
        // I8: normalized → signed normalized formats; not normalized → signed integer formats.
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Scalar, true) => {
            VertexFormat::Snorm8
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Scalar, false) => {
            VertexFormat::Sint8
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Vec2, true) => {
            VertexFormat::Snorm8x2
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Vec2, false) => {
            VertexFormat::Sint8x2
        }
        // Vec3 is not directly supported; pad to 4.
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Vec3, true) => {
            VertexFormat::Snorm8x4
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Vec3, false) => {
            VertexFormat::Sint8x4
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Vec4, true) => {
            VertexFormat::Snorm8x4
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Vec4, false) => {
            VertexFormat::Sint8x4
        }
        // For matrices, treat as the corresponding vector type (i.e. a Mat2 becomes a Vec2, etc.)
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Mat2, true) => {
            VertexFormat::Snorm8x2
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Mat2, false) => {
            VertexFormat::Sint8x2
        }
        // Mat3 has 3 columns; pad each column to 4 components.
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Mat3, true) => {
            VertexFormat::Snorm8x4
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Mat3, false) => {
            VertexFormat::Sint8x4
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Mat4, true) => {
            VertexFormat::Snorm8x4
        }
        (gltf::accessor::DataType::I8, gltf::accessor::Dimensions::Mat4, false) => {
            VertexFormat::Sint8x4
        }

        // U8: normalized → unsigned normalized formats; not normalized → unsigned integer formats.
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Scalar, true) => {
            VertexFormat::Unorm8
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Scalar, false) => {
            VertexFormat::Uint8
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Vec2, true) => {
            VertexFormat::Unorm8x2
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Vec2, false) => {
            VertexFormat::Uint8x2
        }
        // Vec3: pad to 4.
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Vec3, true) => {
            VertexFormat::Unorm8x4
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Vec3, false) => {
            VertexFormat::Uint8x4
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Vec4, true) => {
            VertexFormat::Unorm8x4
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Vec4, false) => {
            VertexFormat::Uint8x4
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Mat2, true) => {
            VertexFormat::Unorm8x2
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Mat2, false) => {
            VertexFormat::Uint8x2
        }
        // Mat3: pad to 4.
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Mat3, true) => {
            VertexFormat::Unorm8x4
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Mat3, false) => {
            VertexFormat::Uint8x4
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Mat4, true) => {
            VertexFormat::Unorm8x4
        }
        (gltf::accessor::DataType::U8, gltf::accessor::Dimensions::Mat4, false) => {
            VertexFormat::Uint8x4
        }

        // I16: normalized → signed normalized; not normalized → signed integer.
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Scalar, true) => {
            VertexFormat::Snorm16
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Scalar, false) => {
            VertexFormat::Sint16
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Vec2, true) => {
            VertexFormat::Snorm16x2
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Vec2, false) => {
            VertexFormat::Sint16x2
        }
        // Vec3: pad to 4.
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Vec3, true) => {
            VertexFormat::Snorm16x4
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Vec3, false) => {
            VertexFormat::Sint16x4
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Vec4, true) => {
            VertexFormat::Snorm16x4
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Vec4, false) => {
            VertexFormat::Sint16x4
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Mat2, true) => {
            VertexFormat::Snorm16x2
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Mat2, false) => {
            VertexFormat::Sint16x2
        }
        // Mat3: pad to 4.
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Mat3, true) => {
            VertexFormat::Snorm16x4
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Mat3, false) => {
            VertexFormat::Sint16x4
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Mat4, true) => {
            VertexFormat::Snorm16x4
        }
        (gltf::accessor::DataType::I16, gltf::accessor::Dimensions::Mat4, false) => {
            VertexFormat::Sint16x4
        }

        // U16: normalized → unsigned normalized; not normalized → unsigned integer.
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Scalar, true) => {
            VertexFormat::Unorm16
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Scalar, false) => {
            VertexFormat::Uint16
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Vec2, true) => {
            VertexFormat::Unorm16x2
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Vec2, false) => {
            VertexFormat::Uint16x2
        }
        // Vec3: pad to 4.
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Vec3, true) => {
            VertexFormat::Unorm16x4
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Vec3, false) => {
            VertexFormat::Uint16x4
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Vec4, true) => {
            VertexFormat::Unorm16x4
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Vec4, false) => {
            VertexFormat::Uint16x4
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Mat2, true) => {
            VertexFormat::Unorm16x2
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Mat2, false) => {
            VertexFormat::Uint16x2
        }
        // Mat3: pad to 4.
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Mat3, true) => {
            VertexFormat::Unorm16x4
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Mat3, false) => {
            VertexFormat::Uint16x4
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Mat4, true) => {
            VertexFormat::Unorm16x4
        }
        (gltf::accessor::DataType::U16, gltf::accessor::Dimensions::Mat4, false) => {
            VertexFormat::Uint16x4
        }

        // U32: normalized flag is ignored.
        (gltf::accessor::DataType::U32, gltf::accessor::Dimensions::Scalar, _) => {
            VertexFormat::Uint32
        }
        (gltf::accessor::DataType::U32, gltf::accessor::Dimensions::Vec2, _) => {
            VertexFormat::Uint32x2
        }
        (gltf::accessor::DataType::U32, gltf::accessor::Dimensions::Vec3, _) => {
            VertexFormat::Uint32x3
        }
        (gltf::accessor::DataType::U32, gltf::accessor::Dimensions::Vec4, _) => {
            VertexFormat::Uint32x4
        }
        (gltf::accessor::DataType::U32, gltf::accessor::Dimensions::Mat2, _) => {
            VertexFormat::Uint32x2
        }
        (gltf::accessor::DataType::U32, gltf::accessor::Dimensions::Mat3, _) => {
            VertexFormat::Uint32x3
        }
        (gltf::accessor::DataType::U32, gltf::accessor::Dimensions::Mat4, _) => {
            VertexFormat::Uint32x4
        }

        // F32: normalized flag is ignored.
        (gltf::accessor::DataType::F32, gltf::accessor::Dimensions::Scalar, _) => {
            VertexFormat::Float32
        }
        (gltf::accessor::DataType::F32, gltf::accessor::Dimensions::Vec2, _) => {
            VertexFormat::Float32x2
        }
        (gltf::accessor::DataType::F32, gltf::accessor::Dimensions::Vec3, _) => {
            VertexFormat::Float32x3
        }
        (gltf::accessor::DataType::F32, gltf::accessor::Dimensions::Vec4, _) => {
            VertexFormat::Float32x4
        }
        (gltf::accessor::DataType::F32, gltf::accessor::Dimensions::Mat2, _) => {
            VertexFormat::Float32x2
        }
        (gltf::accessor::DataType::F32, gltf::accessor::Dimensions::Mat3, _) => {
            VertexFormat::Float32x3
        }
        (gltf::accessor::DataType::F32, gltf::accessor::Dimensions::Mat4, _) => {
            VertexFormat::Float32x4
        }
    }
}
