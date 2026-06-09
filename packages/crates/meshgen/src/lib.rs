//! Pure-CPU mesh + texture-pixel generators. See [`README.md`](../README.md).
//!
//! Feature-gated so the player carries only what it runs: `primitives` +
//! `mesh_data` are always compiled (glam-only); the recipe **types** (`recipes`)
//! and the modifier/SDF/sweep **execution** + its heavy deps (`authoring`) are
//! opt-in. A plain `awsm-meshgen` dependency is the player-lean build.

pub mod mesh_data;
pub mod primitives;

#[cfg(feature = "recipes")]
pub mod recipe;

#[cfg(feature = "authoring")]
pub mod edit;
#[cfg(feature = "authoring")]
pub mod expr;
#[cfg(feature = "authoring")]
pub mod modifiers;
#[cfg(feature = "authoring")]
pub mod procedural_texture;
#[cfg(feature = "authoring")]
pub mod sdf;
#[cfg(feature = "authoring")]
pub mod sdf_mesh;
#[cfg(feature = "authoring")]
pub mod stats;
#[cfg(feature = "authoring")]
pub mod sweep;

pub use mesh_data::{compute_vertex_normals, MeshData};
pub use primitives::{
    box_mesh, cone_mesh, cylinder_mesh, plane_mesh, sphere_mesh, sprite_quad, torus_mesh,
};

#[cfg(feature = "recipes")]
pub use recipe::*;

#[cfg(feature = "authoring")]
pub use modifiers::{apply_modifiers, evaluate, lathe, superquadric};
#[cfg(feature = "authoring")]
pub use procedural_texture::{checker_rgba, gradient_rgba, noise_rgba};
#[cfg(feature = "authoring")]
pub use stats::{cross_section_profile, mesh_stats, MeshStats};
#[cfg(feature = "authoring")]
pub use sweep::{sweep_along_curve, CrossSection, SweepOpts, UvMode};
