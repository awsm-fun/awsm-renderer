//! `rstar::SelectionFunction` impl that prunes the BVH by a 6-plane frustum.
//!
//! The R*-tree calls `should_unpack_parent` on every inner-node envelope.
//! Returning `false` skips the entire sub-tree — that's the pruning that
//! makes a hierarchical sweep cheaper than the previous linear walk. Leaf
//! tests then run the same plane-vs-AABB check `Frustum::intersects_aabb`
//! does, so the surviving set is identical to the old linear path.

use glam::Vec3;
use rstar::{
    primitives::{GeomWithData, Rectangle as RstarRectangle},
    SelectionFunction, AABB as RstarAABB,
};

use crate::{frustum::Frustum, meshes::MeshKey};

use super::node::SceneNode;

/// Leaf shape stored in the rstar `RTree`. The `Rectangle` primitive is
/// the wrapper that implements `RTreeObject` over a `[f32; 3]` AABB —
/// the bare `rstar::AABB` is only an envelope type, not an inhabitant.
pub(crate) type Leaf = GeomWithData<RstarRectangle<[f32; 3]>, MeshKey>;

pub(crate) struct FrustumSelector {
    pub(crate) frustum: Frustum,
}

impl FrustumSelector {
    fn rstar_envelope_intersects(&self, envelope: &RstarAABB<[f32; 3]>) -> bool {
        let lower = envelope.lower();
        let upper = envelope.upper();
        intersects(
            self.frustum,
            Vec3::new(lower[0], lower[1], lower[2]),
            Vec3::new(upper[0], upper[1], upper[2]),
        )
    }
}

impl SelectionFunction<Leaf> for FrustumSelector {
    fn should_unpack_parent(&self, envelope: &RstarAABB<[f32; 3]>) -> bool {
        self.rstar_envelope_intersects(envelope)
    }

    fn should_unpack_leaf(&self, leaf: &Leaf) -> bool {
        let rect = leaf.geom();
        let min = rect.lower();
        let max = rect.upper();
        intersects(
            self.frustum,
            Vec3::new(min[0], min[1], min[2]),
            Vec3::new(max[0], max[1], max[2]),
        )
    }
}

/// Returns true when the AABB defined by `min`/`max` is at least partially
/// inside `frustum`. Identical predicate to
/// [`Frustum::intersects_aabb`](crate::frustum::Frustum::intersects_aabb)
/// but spelled out on raw vectors to avoid an `Aabb` rebuild per inner node.
pub(crate) fn intersects(frustum: Frustum, min: Vec3, max: Vec3) -> bool {
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

/// Convenience predicate used by the dynamic-sidecar linear scan.
pub(crate) fn frustum_intersects_node(frustum: &Frustum, node: &SceneNode) -> bool {
    intersects(*frustum, node.aabb.min, node.aabb.max)
}
