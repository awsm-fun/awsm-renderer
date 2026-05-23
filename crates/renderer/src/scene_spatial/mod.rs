//! Spatial index for renderer-owned scene queries.
//!
//! The renderer owns a single source-of-truth BVH (currently `rstar`'s
//! R*-tree) over every mesh's world-space AABB. Per-view frustum culling
//! (camera + shadows) and per-light AABB-overlap queries all descend
//! through this index instead of linearly walking every mesh. External
//! crates (physics, AI) consume a narrow read-only [`SpatialQuery`]
//! trait — they never mutate the index.

mod api;
mod index;
mod node;
mod query;

pub mod frustum_selector;

#[cfg(test)]
mod tests;

pub use index::SceneSpatial;
pub use node::{SceneNode, SceneNodeFlags};
pub use query::{NodeFilter, SpatialQuery};
