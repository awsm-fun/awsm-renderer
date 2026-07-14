//! Bounding-box helpers for parsed glTF documents.
//!
//! Lives in this crate (not `awsm-renderer`) so the renderer's `Aabb` type
//! doesn't have to depend on the `gltf` crate.

use awsm_renderer::bounds::Aabb;
use glam::{Mat4, Vec3};

/// Computes an AABB enclosing every primitive in a glTF document, walking
/// each scene + transform-composing per node. Returns a unit cube if the
/// document carries no geometry.
pub fn aabb_from_gltf_doc(doc: &gltf::Document) -> Aabb {
    let mut aabb: Option<Aabb> = None;

    fn process_node(node: &gltf::Node, parent_transform: Mat4, aabb: &mut Option<Aabb>) {
        if let Some(mesh_aabb) = aabb_from_gltf_node(node, Some(parent_transform)) {
            match aabb {
                Some(existing) => existing.extend(&mesh_aabb),
                None => *aabb = Some(mesh_aabb),
            }
        }

        let new_parent_transform =
            parent_transform * Mat4::from_cols_array_2d(&node.transform().matrix());
        for child in node.children() {
            process_node(&child, new_parent_transform, aabb);
        }
    }

    for scene in doc.scenes() {
        for node in scene.nodes() {
            process_node(&node, Mat4::IDENTITY, &mut aabb);
        }
    }

    aabb.unwrap_or_else(Aabb::new_unit_cube)
}

/// Computes an AABB for a single glTF node (recursive into primitives but
/// not into children). `parent_transform` is the accumulated world-space
/// transform of the parent chain.
pub fn aabb_from_gltf_node(node: &gltf::Node, parent_transform: Option<Mat4>) -> Option<Aabb> {
    let node_transform = match parent_transform {
        Some(transform) => transform * Mat4::from_cols_array_2d(&node.transform().matrix()),
        None => Mat4::from_cols_array_2d(&node.transform().matrix()),
    };

    let mut aabb: Option<Aabb> = None;

    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            if let Some(primitive_aabb) = aabb_from_gltf_primitive(&primitive, Some(node_transform))
            {
                match aabb {
                    Some(ref mut existing) => existing.extend(&primitive_aabb),
                    None => aabb = Some(primitive_aabb),
                }
            }
        }
    }

    aabb
}

/// Computes an AABB for a glTF primitive, reading the position accessor's
/// declared min/max bounds (no buffer walk required).
pub fn aabb_from_gltf_primitive(
    primitive: &gltf::Primitive,
    transform: Option<Mat4>,
) -> Option<Aabb> {
    let position_accessor = primitive.get(&gltf::Semantic::Positions)?;
    let (min, max) = position_min_max(&position_accessor)?;

    let mut mesh_aabb = Aabb::new(min, max);
    if let Some(transform) = transform {
        mesh_aabb.transform(&transform);
    }
    Some(mesh_aabb)
}

/// The position accessor's declared min/max as dequantized f32 — shared by
/// every AABB reader in this crate.
///
/// Per the glTF spec, `accessor.min`/`max` hold values in the accessor's
/// COMPONENT TYPE: a `KHR_mesh_quantization` normalized-i16 POSITION declares
/// integer bounds up to ±32767. Reading them raw inflates the AABB by the
/// quantization divisor (breaking culling / LOD radii / camera framing), so
/// normalized integer bounds are divided down here, mirroring the attribute
/// path's dequantization; unnormalized quantized positions are used as-is
/// (their scale rides the node TRS / IBMs).
pub(crate) fn position_min_max(accessor: &gltf::Accessor<'_>) -> Option<(Vec3, Vec3)> {
    use gltf::accessor::DataType;

    let (min_val, max_val) = (accessor.min()?, accessor.max()?);
    let (min_arr, max_arr) = (min_val.as_array()?, max_val.as_array()?);
    if min_arr.len() != 3 || max_arr.len() != 3 {
        return None;
    }

    let dequant: fn(f64) -> f32 = if accessor.normalized() {
        match accessor.data_type() {
            DataType::I8 => |v| (v as f32 / 127.0).max(-1.0),
            DataType::U8 => |v| v as f32 / 255.0,
            DataType::I16 => |v| (v as f32 / 32767.0).max(-1.0),
            DataType::U16 => |v| v as f32 / 65535.0,
            _ => |v| v as f32,
        }
    } else {
        |v| v as f32
    };

    let read = |arr: &[gltf::json::Value]| -> Option<Vec3> {
        Some(Vec3::new(
            dequant(arr[0].as_f64()?),
            dequant(arr[1].as_f64()?),
            dequant(arr[2].as_f64()?),
        ))
    };
    Some((read(min_arr)?, read(max_arr)?))
}

#[cfg(all(test, has_local_fixtures))]
mod fixture_tests {
    use super::*;

    const POLICE_GLB: &[u8] = include_bytes!("../../../../fixtures/local/police-meshopt.glb");

    /// The quantized robot's document AABB must come out at model scale, not
    /// quantized-integer scale: raw normalized-i16 min/max are ±32767, so a
    /// missed dequantization inflates the box by that factor.
    #[test]
    fn quantized_position_bounds_dequantize() {
        let gltf = crate::loader::parse_gltf_lenient(POLICE_GLB).unwrap();
        let aabb = aabb_from_gltf_doc(&gltf.document);
        let size = aabb.size();
        assert!(
            size.max_element() > 0.01 && size.max_element() < 100.0,
            "robot AABB should be model-scale, got {size:?}"
        );
    }
}
