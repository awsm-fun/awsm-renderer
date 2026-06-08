//! Renderer bridge: mirrors the reactive scene tree onto the GPU renderer.
//! A per-node `RendererNode` tracks the GPU resources a scene node owns
//! (transform, meshes, materials, light); observers materialize/teardown them
//! as the node's kind/transform/visibility change. Primitives, lights, and
//! passive kinds are handled; models/curves/particles/decals/etc. layer in.

pub mod animation_sync;
pub mod asset_template;
pub mod collider_wire;
pub mod dynamic;
pub mod env_sync;
pub mod gltf;
pub mod material;
pub mod mesh_cache;
pub mod mesh_sync;
pub mod node_sync;
pub mod particles;
pub mod skin_bridge;
mod state;

pub use state::*;
