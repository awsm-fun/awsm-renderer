//! Quantization-aware attribute reads (KHR_mesh_quantization).
//!
//! The gltf crate's typed readers (`read_positions` / `read_normals` /
//! `read_tangents`) assert the accessor's component type is F32 and panic on
//! quantized models (i16/i8 normalized). These helpers read any component
//! type and dequantize normalized integers with the standard glTF divisors,
//! byte-for-byte matching the renderer's attribute path.

use gltf::accessor::DataType;

/// Read a `VEC<N>` attribute as f32, dequantizing normalized integer
/// component types. Returns `None` when the semantic is absent, its
/// dimensionality doesn't match `N`, or the accessor has no bufferView
/// (sparse-only accessors don't appear in our quantized inputs).
pub(crate) fn read_attr_f32<const N: usize>(
    primitive: &gltf::Primitive<'_>,
    semantic: &gltf::Semantic,
    buffers: &[Vec<u8>],
) -> Option<Vec<[f32; N]>> {
    let accessor = primitive.get(semantic)?;
    if accessor.dimensions().multiplicity() != N {
        return None;
    }
    let view = accessor.view()?;
    let data_type = accessor.data_type();
    let normalized = accessor.normalized();
    let elem_size = accessor.size();
    let stride = view.stride().unwrap_or(elem_size);
    let buf = buffers.get(view.buffer().index())?;
    let base = view.offset() + accessor.offset();
    let comp_size = data_type.size();

    let mut out = Vec::with_capacity(accessor.count());
    for i in 0..accessor.count() {
        let elem = base + i * stride;
        let mut value = [0f32; N];
        for (c, slot) in value.iter_mut().enumerate() {
            let at = elem + c * comp_size;
            let bytes = buf.get(at..at + comp_size)?;
            *slot = match data_type {
                DataType::F32 => f32::from_le_bytes(bytes.try_into().ok()?),
                DataType::U8 => {
                    let v = bytes[0];
                    if normalized {
                        v as f32 / 255.0
                    } else {
                        v as f32
                    }
                }
                DataType::I8 => {
                    let v = bytes[0] as i8;
                    if normalized {
                        (v as f32 / 127.0).max(-1.0)
                    } else {
                        v as f32
                    }
                }
                DataType::U16 => {
                    let v = u16::from_le_bytes(bytes.try_into().ok()?);
                    if normalized {
                        v as f32 / 65535.0
                    } else {
                        v as f32
                    }
                }
                DataType::I16 => {
                    let v = i16::from_le_bytes(bytes.try_into().ok()?);
                    if normalized {
                        (v as f32 / 32767.0).max(-1.0)
                    } else {
                        v as f32
                    }
                }
                DataType::U32 => u32::from_le_bytes(bytes.try_into().ok()?) as f32,
            };
        }
        out.push(value);
    }
    Some(out)
}
