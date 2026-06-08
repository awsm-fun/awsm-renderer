//! Pure-CPU mesh + texture-pixel generators. See [`README.md`](../README.md).

pub mod edit;
pub mod expr;
pub mod mesh_data;
pub mod modifiers;
pub mod primitives;
pub mod procedural_texture;
pub mod sdf;
pub mod stats;
pub mod sweep;

pub use mesh_data::{compute_vertex_normals, MeshData};
pub use modifiers::{apply_modifiers, evaluate, lathe, superquadric};
pub use primitives::{
    box_mesh, cone_mesh, cylinder_mesh, plane_mesh, sphere_mesh, sprite_quad, torus_mesh,
};
pub use procedural_texture::{checker_rgba, gradient_rgba, noise_rgba};
pub use stats::{cross_section_profile, mesh_stats, MeshStats};
pub use sweep::{sweep_along_curve, CrossSection, SweepOpts, UvMode};
