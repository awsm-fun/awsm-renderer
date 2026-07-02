//! Spatial index for renderer-owned scene queries.
//!
//! The renderer owns a single source-of-truth BVH (parry3d's dynamic
//! `Bvh`) over every mesh's world-space AABB — static and per-frame-
//! moving meshes alike (leaf updates are incremental; a fattening margin
//! absorbs small motion, and a once-per-frame refit + incremental
//! rebalance keeps quality without full rebuilds). Per-view frustum
//! culling (camera + shadows) and per-light AABB-overlap queries all
//! descend through this index instead of linearly walking every mesh.
//!
//! This is a rendering index: gameplay/physics spatial queries belong to
//! the host app's physics engine, not here.

mod api;
mod index;
mod node;
mod query;

#[cfg(test)]
mod tests;

pub use index::{SceneSpatial, SceneSpatialConfig};
pub use node::{SceneNode, SceneNodeFlags};
pub use query::NodeFilter;
