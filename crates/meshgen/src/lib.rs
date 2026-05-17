//! Pure-CPU mesh + texture-pixel generators. See [`README.md`](../README.md).

pub mod mesh_data;
pub mod primitives;
pub mod procedural_texture;
pub mod sweep;

pub use mesh_data::{compute_vertex_normals, MeshData};
pub use primitives::{
    box_mesh, cone_mesh, cylinder_mesh, plane_mesh, sphere_mesh, sprite_quad, torus_mesh,
};
pub use procedural_texture::{checker_rgba, gradient_rgba, noise_rgba};
pub use sweep::{sweep_along_curve, CrossSection, SweepOpts, UvMode};
