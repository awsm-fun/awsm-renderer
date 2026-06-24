//! `awsm-renderer-lod-bake` — offline LOD bake over plain geometry arrays.
//!
//! Pure Rust, no GPU, **wasm-safe** (the bake runs inside the editor frontend,
//! a `wasm32-unknown-unknown` crate, so a C simplifier like meshoptimizer is not
//! an option — Apple clang has no wasm target). The shared primitive is a
//! boundary-locked half-edge QEM collapse ([`simplify`]) that keeps the
//! surviving vertices a *subset* of the originals, so per-vertex attributes
//! (skin weights, morph deltas, …) carry through a level verbatim.
//!
//! Phase A (discrete LOD chain) uses [`build_lod_chain`]; Phase B (cluster LOD
//! DAG) will reuse the same collapse with locked group boundaries.

pub mod cluster;
pub mod cluster_mesh;
pub mod dag;
pub mod manifest;
pub mod plan;
pub mod quadric;
pub mod simplify;

pub use cluster::{build_cluster_graph, build_clusters, group_clusters, ClusterGraph, Meshlet};
pub use cluster_mesh::{ClusterMesh, ClusterPage};
pub use dag::{build_cluster_dag, ClusterDag, DagCluster, DagOptions, ROOT_PARENT_ERROR};
pub use manifest::{
    bounding_sphere_radius, lod_level_filename, lod_manifest_filename, MeshLodLevel, MeshLodManifest,
};
pub use plan::{plan_lod_levels, LodPlan, PlannedLevel};
pub use simplify::{build_lod_chain, simplify, SimplifiedMesh, SimplifyOptions};

#[cfg(test)]
mod tests {
    use super::simplify::*;

    /// A flat W×H grid of quads (two triangles each) on the z=0 plane. Returns
    /// (positions, indices). The outer ring is the boundary (locked); interior
    /// vertices are free to collapse.
    fn grid(w: usize, h: usize) -> (Vec<[f32; 3]>, Vec<u32>) {
        let mut pos = Vec::new();
        for y in 0..=h {
            for x in 0..=w {
                pos.push([x as f32, y as f32, 0.0]);
            }
        }
        let idx = |x: usize, y: usize| (y * (w + 1) + x) as u32;
        let mut indices = Vec::new();
        for y in 0..h {
            for x in 0..w {
                indices.extend_from_slice(&[idx(x, y), idx(x + 1, y), idx(x + 1, y + 1)]);
                indices.extend_from_slice(&[idx(x, y), idx(x + 1, y + 1), idx(x, y + 1)]);
            }
        }
        (pos, indices)
    }

    fn assert_indices_in_range(m: &SimplifiedMesh) {
        let n = m.surviving.len() as u32;
        for &i in &m.indices {
            assert!(i < n, "index {i} out of range for {n} surviving verts");
        }
        // No degenerate triangles.
        for t in m.indices.chunks_exact(3) {
            assert!(t[0] != t[1] && t[1] != t[2] && t[0] != t[2]);
        }
    }

    #[test]
    fn flat_grid_simplifies_and_stays_valid() {
        let (pos, indices) = grid(8, 8); // 128 triangles, 81 verts
        let base_tris = indices.len() / 3;
        let m = simplify(&pos, &indices, SimplifyOptions::with_target(base_tris / 4));
        assert_indices_in_range(&m);
        assert!(
            m.triangle_count() < base_tris,
            "expected reduction from {base_tris}, got {}",
            m.triangle_count()
        );
        // A flat plane has zero QEM error no matter how far we collapse.
        assert!(m.error < 1e-3, "flat plane should simplify losslessly, err={}", m.error);
    }

    #[test]
    fn extent_is_preserved_under_moderate_simplification() {
        // Boundary vertices may slide along the seam (and a corner with a single
        // incident triangle can even be orphaned), but locked boundary geometry
        // keeps SOME surviving vertex on each extent edge, so the silhouette
        // still spans the full [0,8]² bounds.
        let (pos, indices) = grid(8, 8); // 128 tris
        let base = indices.len() / 3;
        let m = simplify(&pos, &indices, SimplifyOptions::with_target(base * 3 / 4));
        let xs: Vec<f32> = m.surviving.iter().map(|&s| pos[s as usize][0]).collect();
        let ys: Vec<f32> = m.surviving.iter().map(|&s| pos[s as usize][1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::MAX, f32::min), 0.0);
        assert_eq!(xs.iter().cloned().fold(f32::MIN, f32::max), 8.0);
        assert_eq!(ys.iter().cloned().fold(f32::MAX, f32::min), 0.0);
        assert_eq!(ys.iter().cloned().fold(f32::MIN, f32::max), 8.0);
    }

    /// The simplifier is deterministic: identical input ⇒ identical output (no
    /// HashMap-iteration-order dependence). Required for content-hash-cached bakes.
    #[test]
    fn simplify_is_deterministic() {
        let (pos, indices) = grid(10, 10);
        let opts = SimplifyOptions::with_target(80);
        let a = simplify(&pos, &indices, opts);
        let b = simplify(&pos, &indices, opts);
        assert_eq!(a.surviving, b.surviving);
        assert_eq!(a.indices, b.indices);
        assert_eq!(a.error.to_bits(), b.error.to_bits());
    }

    #[test]
    fn smooth_boundary_simplifies_below_old_lock_floor() {
        // Regression for the over-locking that plateaued seam-heavy meshes: a
        // grid's straight edges are smooth boundary (not corners), so the mesh
        // must now collapse far below the "every boundary vertex locked" floor.
        // Old rule kept the entire 24-vertex boundary ring; the new rule slides
        // it down to ~the 4 corners.
        let (pos, indices) = grid(6, 6); // 72 tris, 24 boundary-ring verts
        let m = simplify(&pos, &indices, SimplifyOptions::with_target(2));
        assert!(
            m.surviving.len() < 12,
            "expected aggressive reduction, kept {} verts",
            m.surviving.len()
        );
    }

    #[test]
    fn gather_carries_attributes_for_survivors() {
        let (pos, indices) = grid(4, 4);
        // A fake attribute: each vertex tagged with its own index.
        let attr: Vec<u32> = (0..pos.len() as u32).collect();
        let m = simplify(&pos, &indices, SimplifyOptions::with_target(8));
        let gathered = m.gather(&attr);
        assert_eq!(gathered.len(), m.surviving.len());
        // gather must return exactly the surviving original ids (subset property).
        assert_eq!(gathered, m.surviving);
    }

    #[test]
    fn target_at_or_above_base_is_identity() {
        let (pos, indices) = grid(3, 3);
        let base = indices.len() / 3;
        let m = simplify(&pos, &indices, SimplifyOptions::with_target(base));
        assert_eq!(m.surviving.len(), pos.len());
        assert_eq!(m.indices, indices);
        assert_eq!(m.error, 0.0);
    }

    #[test]
    fn lod_chain_is_monotonic_in_triangle_count() {
        let (pos, indices) = grid(10, 10); // 200 tris
        let levels = build_lod_chain(&pos, &indices, &[0.5, 0.25, 0.1]);
        assert_eq!(levels.len(), 3);
        let mut prev = indices.len() / 3 + 1;
        for lvl in &levels {
            assert_indices_in_range(lvl);
            assert!(
                lvl.triangle_count() <= prev,
                "levels must be non-increasing in tris: {} then {}",
                prev,
                lvl.triangle_count()
            );
            prev = lvl.triangle_count();
        }
    }

    /// A closed manifold (octahedron) has **no** boundary edges, so no vertex is
    /// locked and the simplifier is free to collapse. It must not panic and must
    /// stay a valid index buffer.
    #[test]
    fn closed_manifold_simplifies_without_panic() {
        let pos = vec![
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        // 8 faces of an octahedron (consistent winding).
        let indices = vec![
            4, 0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 5, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3,
        ];
        let m = simplify(&pos, &indices, SimplifyOptions::with_target(2));
        assert_indices_in_range(&m);
        assert!(m.triangle_count() <= 8);
    }

    /// Messy input — a degenerate (zero-area) triangle, a triangle with a
    /// duplicate index, and an unreferenced vertex — must not panic and must
    /// yield a clean, in-range, degenerate-free index buffer.
    #[test]
    fn messy_input_is_robust() {
        let (mut pos, mut indices) = grid(6, 6);
        // Unreferenced extra vertex.
        pos.push([100.0, 100.0, 100.0]);
        // A degenerate triangle (three collinear / identical-ish points) and a
        // duplicate-index triangle.
        let a = 0u32;
        let b = 1u32;
        indices.extend_from_slice(&[a, a, b]); // duplicate index → degenerate
        indices.extend_from_slice(&[a, b, a]); // also degenerate
        let base = indices.len() / 3;
        let m = simplify(&pos, &indices, SimplifyOptions::with_target(base / 4));
        assert_indices_in_range(&m);
    }

    #[test]
    fn curved_surface_reports_nonzero_error() {
        // A paraboloid grid: collapsing interior verts loses real height, so the
        // QEM error must be > 0 (and the boundary still survives).
        let (mut pos, indices) = grid(8, 8);
        for p in &mut pos {
            let (x, y) = (p[0] - 4.0, p[1] - 4.0);
            p[2] = 0.1 * (x * x + y * y);
        }
        let base = indices.len() / 3;
        let m = simplify(&pos, &indices, SimplifyOptions::with_target(base / 8));
        assert_indices_in_range(&m);
        assert!(m.error > 0.0, "curved surface must report nonzero error");
    }
}
