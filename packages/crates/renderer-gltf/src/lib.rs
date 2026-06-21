//! glTF ingestion for `awsm-renderer`.
//!
//! Extracted from `awsm-renderer` as part of the editor-renderer overhaul.
//! glTF is one ingestion path into the renderer's public raw-mesh API; it
//! is not privileged. Any other format (USD, custom, procedural) consumes
//! the same renderer surface this crate uses.
//!
//! Loading flow:
//! 1. `loader::load_gltf` (async) fetches `.gltf` / `.glb` + external
//!    buffers / images, parses into a `GltfData`.
//! 2. `AwsmRenderer::populate_gltf(...)` (via the `AwsmRendererGltfExt`
//!    extension trait below) walks the parsed scene and uploads meshes,
//!    materials, textures, animations, skins, transforms into the renderer.
//! 3. The caller gets back a `GltfKeyLookups` mapping per-document glTF
//!    indices to renderer keys (mesh / material / transform / animation).

pub mod aabb;
pub mod buffers;
pub mod data;
pub mod error;
pub mod ext;
pub mod extract;
pub mod loader;
pub mod populate;
pub mod worker_job;

pub use aabb::{aabb_from_gltf_doc, aabb_from_gltf_node, aabb_from_gltf_primitive};
pub use ext::AwsmRendererGltfExt;
pub use extract::{extract_animations, ExtractedAnimation, ExtractedChannel, ExtractedProperty};
pub use populate::{GltfKeyLookups, GltfMaterialSource, GltfPopulateContext, PopulateGltfOpts};
// Crate-root re-exports of the types every consumer touches on the load path, so a
// player can `use awsm_renderer_gltf::{GltfLoader, GltfData, populate_gltf}` without
// reaching into submodules. (Ergonomic form: `renderer.populate_gltf(data, scene)` via
// [`AwsmRendererGltfExt`]; `populate_gltf` is the equivalent free function.)
pub use data::GltfData;
pub use loader::GltfLoader;
pub use populate::populate_gltf;
