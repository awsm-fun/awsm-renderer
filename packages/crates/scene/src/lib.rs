//! `awsm-renderer-scene` — the lean, canonical **runtime** scene schema for the
//! awsm-renderer player: a [`Scene`] (`scene.toml`) + an `assets/` directory,
//! all by-id. The player and renderer touch only this crate.
//!
//! Authoring lives elsewhere: the modifier stack + per-vertex overrides + the
//! editor's `Mesh = base + edits` are in `awsm-renderer-meshgen` (recipe types) and
//! `awsm-renderer-editor-protocol` (the `EditorProject` document + `EditorCommand`/
//! `EditorQuery`), which depend on this crate and reuse its core types. The
//! editor's bake step lowers authoring → runtime (`MeshDef` → [`mesh::MeshBlob`]).
//!
//! Coordinate convention: right-handed, Y-up, meters. Rotations are unit
//! quaternions stored as `[x, y, z, w]`.

pub mod animation;
pub mod assets;
pub mod camera;
pub mod collider;
pub mod curve;
pub mod decal;
pub mod dynamic_material;
pub mod environment;
pub mod instances;
pub mod light;
pub mod line;
pub mod material;
pub mod mesh;
pub mod particle;
pub mod primitive;
pub mod project_dir;
pub mod scene;
pub mod shadows;
pub mod sprite;
pub mod transform;
pub mod tree;

pub use animation::*;
pub use assets::*;
pub use camera::*;
pub use collider::*;
pub use curve::*;
pub use decal::*;
pub use dynamic_material::*;
pub use environment::*;
pub use instances::*;
pub use light::*;
pub use line::*;
pub use material::*;
pub use mesh::*;
pub use particle::*;
pub use primitive::*;
pub use project_dir::*;
pub use scene::*;
pub use shadows::*;
pub use sprite::*;
pub use transform::*;
pub use tree::*;
