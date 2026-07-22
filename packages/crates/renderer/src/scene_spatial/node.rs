//! Leaf payload stored in the scene BVH, plus the glam↔parry conversion
//! seam.
//!
//! The renderer's math is `glam` (workspace-pinned); parry's `Bvh` speaks
//! its own glam re-export (via `glamx`), which may trail or lead the
//! workspace version. All conversion between the two happens HERE, on
//! plain `f32` components, so a glam version skew can never leak type
//! errors into the rest of the crate.

use glam::Vec3;

use crate::{bounds::Aabb, frustum::Frustum, meshes::MeshKey};

/// Per-mesh flags consulted by query-time filters.
///
/// Mirrors the fields of `Mesh` (`cast_shadows`, `receive_shadows`, `hidden`,
/// `hud`). Flag flips on a live node are applied in place by
/// [`SceneSpatial::set_flags`](super::SceneSpatial::set_flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SceneNodeFlags {
    pub cast_shadows: bool,
    pub receive_shadows: bool,
    pub hidden: bool,
    pub hud: bool,
    /// Whether the mesh's material renders in the TRANSPARENCY pass
    /// (alpha-BLEND — Mask is alpha-tested opaque and stays `false`).
    /// Mirrored from `Materials::is_transparency_pass(mesh.material_key)`
    /// by `sync_spatial_for_mesh`. The shadow-caster filter excludes these
    /// by default: the shadow pass has no blend representation, so a glass
    /// pane would otherwise cast a fully OPAQUE shadow — a light outside a
    /// glazed wall blacks out everything behind the glass (where Blender's
    /// transmissive glass lets the light through).
    pub blend_material: bool,
}

impl SceneNodeFlags {
    /// Builds flags from a `Mesh`. `blend_material` comes from the material
    /// table (the mesh only stores the key), so the caller resolves it.
    pub fn from_mesh(mesh: &crate::meshes::mesh::Mesh, blend_material: bool) -> Self {
        Self {
            cast_shadows: mesh.cast_shadows,
            receive_shadows: mesh.receive_shadows,
            hidden: mesh.hidden,
            hud: mesh.hud,
            blend_material,
        }
    }
}

/// Mesh leaf stored in the spatial index. The `aabb` mirrors the
/// `Mesh::world_aabb` **exactly** (never fattened) so the index can answer
/// queries without a hop through `Meshes` first — the tree's leaves carry
/// margin-dilated boxes for cheap incremental updates, and every query
/// re-tests candidates against this exact AABB before yielding them.
#[derive(Debug, Clone)]
pub struct SceneNode {
    pub aabb: Aabb,
    pub mesh_key: MeshKey,
    pub flags: SceneNodeFlags,
}

/// Our exact [`Aabb`] → a parry AABB (component-wise; see module docs).
pub(crate) fn to_parry_aabb(aabb: &Aabb) -> parry3d::bounding_volume::Aabb {
    parry3d::bounding_volume::Aabb::new(
        parry3d::math::Vector::new(aabb.min.x, aabb.min.y, aabb.min.z),
        parry3d::math::Vector::new(aabb.max.x, aabb.max.y, aabb.max.z),
    )
}

/// Returns true when the AABB defined by `min`/`max` is at least partially
/// inside `frustum`. Identical predicate to
/// [`Frustum::intersects_aabb`](crate::frustum::Frustum::intersects_aabb)
/// but spelled out on raw vectors to avoid an `Aabb` rebuild per BVH node.
pub(crate) fn frustum_intersects(frustum: &Frustum, min: Vec3, max: Vec3) -> bool {
    for plane in &frustum.planes {
        let px = if plane.normal.x >= 0.0 { max.x } else { min.x };
        let py = if plane.normal.y >= 0.0 { max.y } else { min.y };
        let pz = if plane.normal.z >= 0.0 { max.z } else { min.z };
        if plane.distance(Vec3::new(px, py, pz)) < 0.0 {
            return false;
        }
    }
    true
}

/// [`frustum_intersects`] against a parry (fattened, tree-side) AABB.
pub(crate) fn frustum_intersects_parry(
    frustum: &Frustum,
    aabb: &parry3d::bounding_volume::Aabb,
) -> bool {
    frustum_intersects(
        frustum,
        Vec3::new(aabb.mins.x, aabb.mins.y, aabb.mins.z),
        Vec3::new(aabb.maxs.x, aabb.maxs.y, aabb.maxs.z),
    )
}
