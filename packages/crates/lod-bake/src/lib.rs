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

pub mod quadric;
pub mod simplify;

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
    fn boundary_vertices_survive() {
        // Every outer-ring vertex is on a boundary edge and must survive, so the
        // simplified mesh still spans the full extent.
        let (pos, indices) = grid(6, 6);
        let m = simplify(&pos, &indices, SimplifyOptions::with_target(1));
        let survives = |p: [f32; 3]| {
            m.surviving
                .iter()
                .any(|&s| pos[s as usize] == p)
        };
        assert!(survives([0.0, 0.0, 0.0]), "corner must survive");
        assert!(survives([6.0, 6.0, 0.0]), "far corner must survive");
        assert!(survives([3.0, 0.0, 0.0]), "edge-midpoint (boundary) must survive");
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
