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
    let (min_val, max_val) = (position_accessor.min()?, position_accessor.max()?);
    let (min_arr, max_arr) = (min_val.as_array()?, max_val.as_array()?);
    if min_arr.len() != 3 || max_arr.len() != 3 {
        return None;
    }

    let min = Vec3::new(
        min_arr[0].as_f64()? as f32,
        min_arr[1].as_f64()? as f32,
        min_arr[2].as_f64()? as f32,
    );
    let max = Vec3::new(
        max_arr[0].as_f64()? as f32,
        max_arr[1].as_f64()? as f32,
        max_arr[2].as_f64()? as f32,
    );

    let mut mesh_aabb = Aabb::new(min, max);
    if let Some(transform) = transform {
        mesh_aabb.transform(&transform);
    }
    Some(mesh_aabb)
}
