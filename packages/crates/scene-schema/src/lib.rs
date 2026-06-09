//! Authored scene shape consumed by the awsm-renderer scene editor and
//! the runtime player. Pure data — no rendering deps.
//!
//! `EditorProject` is the on-disk schema the editor saves and loads
//! (`project.json`), and the same structure that gets embedded into a
//! per-game build artifact. Treating it as one type means the runtime
//! never juggles "json plus bytes" — Build produces a single bundled
//! struct, Load round-trips it identically.
//!
//! Contents are deliberately game-agnostic: the editor authors a generic
//! scene (nodes, transforms, lights, collisions, environment) plus a
//! flat asset table. Per-game packing rules live in each game's struct
//! and run at Build time, not here.
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
pub mod modifier;
pub mod particle;
pub mod primitive;
pub mod project;
pub mod shadows;
pub mod sprite;
pub mod transform;
pub mod tree;

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
pub use modifier::*;
pub use particle::*;
pub use primitive::*;
pub use project::*;
pub use shadows::*;
pub use sprite::*;
pub use transform::*;
pub use tree::*;

#[cfg(test)]
mod tests;
