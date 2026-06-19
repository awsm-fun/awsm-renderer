//! SDF → triangles via surface nets (the meshing the spec delegates to a crate;
//! `fast-surface-nets`). We sample [`eval_sdf`](crate::sdf::eval_sdf) over a
//! padded grid sized from [`sdf_bounds`](crate::sdf::sdf_bounds), run surface
//! nets, and map the array-local vertices back to world space.

use crate::recipe::SdfNode;
use fast_surface_nets::{surface_nets, SurfaceNetsBuffer};
use glam::Vec3;
use ndshape::{RuntimeShape, Shape};

use crate::mesh_data::MeshData;
use crate::sdf::{eval_sdf, sdf_bounds};

/// Mesh an SDF graph at `resolution` grid samples per axis (clamped 8..=160).
pub fn surface_nets_mesh(node: &SdfNode, resolution: u32) -> MeshData {
    let n = resolution.clamp(8, 160);
    let shape = RuntimeShape::<u32, 3>::new([n, n, n]);

    // Expand the structural AABB so the surface stays off the 1-voxel border
    // (surface nets needs positive samples around the isosurface).
    let (lo, hi) = sdf_bounds(node);
    let margin = ((hi - lo).max_element() * 0.1).max(0.05);
    let lo = lo - Vec3::splat(margin);
    let hi = hi + Vec3::splat(margin);
    let extent = hi - lo;
    let denom = (n - 1) as f32;

    let size = shape.size() as usize;
    let mut sdf = vec![1.0f32; size];
    for i in 0..size as u32 {
        let [x, y, z] = shape.delinearize(i);
        let t = Vec3::new(x as f32, y as f32, z as f32) / denom;
        sdf[i as usize] = eval_sdf(node, lo + t * extent);
    }

    let mut buffer = SurfaceNetsBuffer::default();
    surface_nets(&sdf, &shape, [0, 0, 0], [n - 1, n - 1, n - 1], &mut buffer);

    // Array-local vertex coords → world.
    let positions: Vec<[f32; 3]> = buffer
        .positions
        .iter()
        .map(|p| (lo + Vec3::new(p[0], p[1], p[2]) / denom * extent).to_array())
        .collect();
    let mut mesh = MeshData {
        positions,
        normals: None,
        uvs: Vec::new(),
        colors: None,
        indices: buffer.indices,
    };
    mesh.compute_vertex_normals();
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{SdfNode as N, SdfPrimitive as P};
    use crate::stats::mesh_stats;

    #[test]
    fn meshes_a_sphere() {
        let s = N::Primitive(P::Sphere { radius: 1.0 });
        let m = surface_nets_mesh(&s, 32);
        let stats = mesh_stats(&m);
        assert!(stats.vertices > 100, "verts {}", stats.vertices);
        assert!(stats.triangles > 100);
        // Bbox ≈ the unit sphere (within a voxel of ±1 on each axis).
        for k in 0..3 {
            assert!(
                (stats.bbox_max[k] - 1.0).abs() < 0.15,
                "max {:?}",
                stats.bbox_max
            );
            assert!(
                (stats.bbox_min[k] + 1.0).abs() < 0.15,
                "min {:?}",
                stats.bbox_min
            );
        }
    }

    #[test]
    fn meshes_a_csg_mug() {
        // Cylinder minus a smaller inner cylinder, union a torus handle.
        let mug = N::Union {
            smooth: 0.05,
            children: vec![
                N::Subtract {
                    smooth: 0.0,
                    children: vec![
                        N::Primitive(P::Cylinder {
                            radius: 1.0,
                            height: 2.0,
                        }),
                        N::Transform {
                            trs: awsm_scene::Trs {
                                translation: [0.0, 0.3, 0.0],
                                ..awsm_scene::Trs::IDENTITY
                            },
                            child: Box::new(N::Primitive(P::Cylinder {
                                radius: 0.8,
                                height: 2.0,
                            })),
                        },
                    ],
                },
                N::Transform {
                    trs: awsm_scene::Trs {
                        translation: [1.2, 0.0, 0.0],
                        ..awsm_scene::Trs::IDENTITY
                    },
                    child: Box::new(N::Primitive(P::Torus {
                        major: 0.5,
                        minor: 0.15,
                    })),
                },
            ],
        };
        let m = surface_nets_mesh(&mug, 48);
        let stats = mesh_stats(&m);
        assert!(
            stats.triangles > 500,
            "a mug should mesh densely; got {}",
            stats.triangles
        );
        // The handle extends past the cup wall in +X.
        assert!(
            stats.bbox_max[0] > 1.3,
            "handle missing: {:?}",
            stats.bbox_max
        );
    }
}
