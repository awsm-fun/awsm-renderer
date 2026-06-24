//! Cluster (meshlet) generation for the cluster-LOD DAG (Phase B).
//!
//! Partitions a mesh's triangles into compact clusters of ~`target_triangles`
//! each (Nanite-style meshlets, ~128 tris). Pure Rust: meshoptimizer's C
//! `buildMeshlets` can't build for the `wasm32-unknown-unknown` editor (Apple
//! clang has no wasm target — the same constraint that made the simplifier
//! pure-Rust). Growth is greedy over edge adjacency, picking the adjacent
//! triangle that shares the most vertices with the cluster so clusters stay
//! spatially compact with a small shared boundary (cheap boundaries → better
//! group simplification later).

use std::collections::{HashMap, HashSet};

use glam::DVec3;

/// A cluster of triangles plus its object-space bounding sphere.
#[derive(Clone, Debug)]
pub struct Meshlet {
    /// Triangle ids (indices into the source triangle list, i.e. `indices`
    /// chunks of 3) that make up this cluster.
    pub triangles: Vec<u32>,
    /// Bounding-sphere centre (object space).
    pub center: [f32; 3],
    /// Bounding-sphere radius.
    pub radius: f32,
}

impl Meshlet {
    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }
}

fn tri_verts(indices: &[u32], tri: u32) -> [u32; 3] {
    let i = tri as usize * 3;
    [indices[i], indices[i + 1], indices[i + 2]]
}

fn edge_key(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Partition `(positions, indices)` into clusters of about `target_triangles`
/// each. Every non-degenerate triangle lands in exactly one cluster. Degenerate
/// triangles (a repeated index) are dropped. `target_triangles` is clamped to
/// at least 1.
pub fn build_clusters(
    positions: &[[f32; 3]],
    indices: &[u32],
    target_triangles: usize,
) -> Vec<Meshlet> {
    let target = target_triangles.max(1);
    let tri_count = indices.len() / 3;
    if tri_count == 0 {
        return Vec::new();
    }

    let pos: Vec<DVec3> = positions
        .iter()
        .map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))
        .collect();

    // Edge → the (up to two, more if non-manifold) triangles using it.
    let mut edge_tris: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    let mut degenerate = vec![false; tri_count];
    for t in 0..tri_count as u32 {
        let [a, b, c] = tri_verts(indices, t);
        if a == b || b == c || a == c {
            degenerate[t as usize] = true;
            continue;
        }
        for (u, v) in [(a, b), (b, c), (c, a)] {
            edge_tris.entry(edge_key(u, v)).or_default().push(t);
        }
    }

    // Per-triangle adjacency (triangles sharing an edge).
    let tri_adjacency = |t: u32| -> Vec<u32> {
        let [a, b, c] = tri_verts(indices, t);
        let mut out = Vec::new();
        for (u, v) in [(a, b), (b, c), (c, a)] {
            if let Some(ts) = edge_tris.get(&edge_key(u, v)) {
                for &other in ts {
                    if other != t && !out.contains(&other) {
                        out.push(other);
                    }
                }
            }
        }
        out
    };

    let mut assigned = degenerate.clone(); // degenerate tris count as "done"
    let mut clusters = Vec::new();

    // Seeds in triangle order — deterministic. (A spatial seed order would give
    // marginally rounder clusters; order is fine for a correct first cut.)
    for seed in 0..tri_count as u32 {
        if assigned[seed as usize] {
            continue;
        }
        let mut cluster_tris: Vec<u32> = vec![seed];
        assigned[seed as usize] = true;
        let mut cluster_verts: HashSet<u32> = tri_verts(indices, seed).into_iter().collect();

        // Frontier = unassigned triangles edge-adjacent to the cluster.
        let mut frontier: HashSet<u32> = tri_adjacency(seed)
            .into_iter()
            .filter(|&t| !assigned[t as usize])
            .collect();

        while cluster_tris.len() < target && !frontier.is_empty() {
            // Pick the frontier triangle sharing the most vertices with the
            // cluster (ties → lowest id for determinism) — keeps it compact.
            let mut best = u32::MAX;
            let mut best_score = -1i32;
            for &t in &frontier {
                let v = tri_verts(indices, t);
                let score = v.iter().filter(|x| cluster_verts.contains(x)).count() as i32;
                if score > best_score || (score == best_score && t < best) {
                    best_score = score;
                    best = t;
                }
            }
            frontier.remove(&best);
            if assigned[best as usize] {
                continue;
            }
            assigned[best as usize] = true;
            cluster_tris.push(best);
            for v in tri_verts(indices, best) {
                cluster_verts.insert(v);
            }
            for adj in tri_adjacency(best) {
                if !assigned[adj as usize] {
                    frontier.insert(adj);
                }
            }
        }

        clusters.push(make_meshlet(cluster_tris, indices, &pos));
    }

    clusters
}

fn make_meshlet(triangles: Vec<u32>, indices: &[u32], pos: &[DVec3]) -> Meshlet {
    // Bounding sphere: centroid of the cluster's vertices, radius = farthest.
    let mut verts: HashSet<u32> = HashSet::new();
    for &t in &triangles {
        for v in tri_verts(indices, t) {
            verts.insert(v);
        }
    }
    let mut center = DVec3::ZERO;
    for &v in &verts {
        center += pos[v as usize];
    }
    if !verts.is_empty() {
        center /= verts.len() as f64;
    }
    let mut r2 = 0.0_f64;
    for &v in &verts {
        r2 = r2.max((pos[v as usize] - center).length_squared());
    }
    Meshlet {
        triangles,
        center: [center.x as f32, center.y as f32, center.z as f32],
        radius: r2.sqrt() as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(n: usize) -> (Vec<[f32; 3]>, Vec<u32>) {
        let mut pos = Vec::new();
        for y in 0..=n {
            for x in 0..=n {
                pos.push([x as f32, y as f32, 0.0]);
            }
        }
        let idx = |x: usize, y: usize| (y * (n + 1) + x) as u32;
        let mut indices = Vec::new();
        for y in 0..n {
            for x in 0..n {
                indices.extend_from_slice(&[idx(x, y), idx(x + 1, y), idx(x + 1, y + 1)]);
                indices.extend_from_slice(&[idx(x, y), idx(x + 1, y + 1), idx(x, y + 1)]);
            }
        }
        (pos, indices)
    }

    /// Every non-degenerate triangle is assigned to exactly one cluster, and
    /// clusters respect the target size.
    #[test]
    fn partition_is_a_cover_and_disjoint() {
        let (pos, indices) = grid(12); // 288 tris
        let total = indices.len() / 3;
        let target = 32;
        let clusters = build_clusters(&pos, &indices, target);
        assert!(!clusters.is_empty());

        let mut seen = vec![false; total];
        let mut count = 0;
        for c in &clusters {
            assert!(c.triangle_count() <= target, "cluster exceeds target");
            assert!(c.triangle_count() >= 1);
            assert!(c.radius > 0.0, "cluster needs a positive bounds radius");
            for &t in &c.triangles {
                assert!(!seen[t as usize], "triangle {t} in two clusters");
                seen[t as usize] = true;
                count += 1;
            }
        }
        assert_eq!(count, total, "every triangle assigned exactly once");
        assert!(seen.iter().all(|&s| s));
        // Roughly total/target clusters (greedy growth can stop short at the
        // boundary, so allow generous slack).
        assert!(clusters.len() >= total / target);
    }

    #[test]
    fn target_above_mesh_yields_one_cluster() {
        let (pos, indices) = grid(4); // 32 tris
        let clusters = build_clusters(&pos, &indices, 10_000);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].triangle_count(), 32);
    }

    #[test]
    fn degenerate_triangles_are_dropped() {
        let (pos, mut indices) = grid(4);
        let base = indices.len() / 3;
        indices.extend_from_slice(&[0, 0, 1]); // degenerate
        let clusters = build_clusters(&pos, &indices, 8);
        let assigned: usize = clusters.iter().map(|c| c.triangle_count()).sum();
        assert_eq!(assigned, base, "degenerate triangle must not be clustered");
    }

    #[test]
    fn empty_mesh_is_empty() {
        assert!(build_clusters(&[], &[], 128).is_empty());
    }
}
