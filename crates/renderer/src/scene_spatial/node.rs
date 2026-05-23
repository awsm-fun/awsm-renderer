//! Leaf payload stored in the scene BVH.

use glam::Vec3;
use rstar::primitives::Rectangle as RstarRectangle;
use rstar::AABB as RstarAABB;

use crate::{bounds::Aabb, meshes::MeshKey};

/// Per-mesh flags consulted by query-time filters.
///
/// Mirrors the fields of `Mesh` (`cast_shadows`, `receive_shadows`, `hidden`,
/// `hud`) and additionally tracks whether the node is animated frequently
/// enough that we want it in the linear-scan sidecar rather than the tree.
/// Flag flips on a live node are applied in place by
/// [`SceneSpatial::set_flags`](super::SceneSpatial::set_flags) — they do not
/// move the node between the tree and the sidecar (only `set_dynamic` does).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SceneNodeFlags {
    pub cast_shadows: bool,
    pub receive_shadows: bool,
    pub hidden: bool,
    pub hud: bool,
    pub dynamic: bool,
}

impl SceneNodeFlags {
    /// Builds flags from a `Mesh`. `dynamic` defaults off; the renderer
    /// flips it explicitly via `SceneSpatial::set_dynamic` once a mesh is
    /// identified as a per-frame mover (skinned, instanced, etc.).
    pub fn from_mesh(mesh: &crate::meshes::mesh::Mesh) -> Self {
        Self {
            cast_shadows: mesh.cast_shadows,
            receive_shadows: mesh.receive_shadows,
            hidden: mesh.hidden,
            hud: mesh.hud,
            dynamic: false,
        }
    }
}

/// Mesh leaf stored in the spatial index. The `aabb` mirrors the
/// `Mesh::world_aabb` so the index can answer queries without a hop
/// through `Meshes` first.
#[derive(Debug, Clone)]
pub struct SceneNode {
    pub aabb: Aabb,
    pub mesh_key: MeshKey,
    pub flags: SceneNodeFlags,
}

impl SceneNode {
    pub(crate) fn rstar_rect(&self) -> RstarRectangle<[f32; 3]> {
        aabb_to_rstar_rect(&self.aabb)
    }
}

pub(crate) fn aabb_to_rstar_rect(aabb: &Aabb) -> RstarRectangle<[f32; 3]> {
    RstarRectangle::from_corners(aabb.min.to_array(), aabb.max.to_array())
}

pub(crate) fn aabb_to_rstar_envelope(aabb: &Aabb) -> RstarAABB<[f32; 3]> {
    RstarAABB::from_corners(aabb.min.to_array(), aabb.max.to_array())
}

#[allow(dead_code)]
pub(crate) fn rstar_envelope_to_aabb(envelope: &RstarAABB<[f32; 3]>) -> Aabb {
    let lower = envelope.lower();
    let upper = envelope.upper();
    Aabb {
        min: Vec3::new(lower[0], lower[1], lower[2]),
        max: Vec3::new(upper[0], upper[1], upper[2]),
    }
}
