//! Per-light → per-mesh AABB-overlap buckets + GPU upload, used by the
//! visibility-buffer-native lighting path.
//!
//! Instead of per-tile / per-cluster light lists (the classical
//! forward+ / clustered-deferred shape), we exploit the visibility
//! buffer's per-pixel mesh identity and build **per-mesh** light
//! lists. For each active punctual light, one
//! `SceneSpatial::query_envelope` produces the meshes it can possibly
//! affect; the transpose then gives every mesh its own short list of
//! overlapping lights.

mod buckets;
mod gpu;

pub use buckets::LightMeshBuckets;
pub use gpu::MeshLightIndicesGpu;
