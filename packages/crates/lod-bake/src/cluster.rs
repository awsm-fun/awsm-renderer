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
    build_clusters_welded(positions, indices, target_triangles, None)
}

/// [`build_clusters`] with an optional position-weld map for ADJACENCY ONLY:
/// `weld[vid]` is the canonical id for coincident positions, so triangles that
/// share a geometric edge through distinct (seam/pole) indices are still seen as
/// adjacent. Degeneracy is judged on the ORIGINAL indices (a zero-area triangle
/// with distinct indices is kept and assigned — coverage is exact); only edges
/// whose two endpoints weld together are skipped (self-loops). See
/// [`crate::dag::DagOptions::weld_eps`].
pub(crate) fn build_clusters_welded(
    positions: &[[f32; 3]],
    indices: &[u32],
    target_triangles: usize,
    weld: Option<&[u32]>,
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

    // Canonical id for edge keys: welded when a map is supplied, else identity.
    let vid = |v: u32| -> u32 {
        match weld {
            Some(w) => w[v as usize],
            None => v,
        }
    };

    // Edge → the (up to two, more if non-manifold) triangles using it.
    let mut edge_tris: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    let mut degenerate = vec![false; tri_count];
    for t in 0..tri_count as u32 {
        let [a, b, c] = tri_verts(indices, t);
        // Degeneracy on ORIGINAL indices → zero-area-but-distinct-index triangles
        // (e.g. UV-sphere pole quads) are KEPT so coverage stays exact.
        if a == b || b == c || a == c {
            degenerate[t as usize] = true;
            continue;
        }
        for (u, v) in [(a, b), (b, c), (c, a)] {
            let (wu, wv) = (vid(u), vid(v));
            // Skip an edge that welds to a self-loop (corners coincident in space).
            if wu == wv {
                continue;
            }
            edge_tris.entry(edge_key(wu, wv)).or_default().push(t);
        }
    }

    // Per-triangle adjacency (triangles sharing a welded edge).
    let tri_adjacency = |t: u32| -> Vec<u32> {
        let [a, b, c] = tri_verts(indices, t);
        let mut out = Vec::new();
        for (u, v) in [(a, b), (b, c), (c, a)] {
            let (wu, wv) = (vid(u), vid(v));
            if wu == wv {
                continue;
            }
            if let Some(ts) = edge_tris.get(&edge_key(wu, wv)) {
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

/// Cluster adjacency graph: for each cluster, its neighbours and the number of
/// edges they share (the shared-boundary weight). Built from per-triangle
/// cluster membership; the `metis` stand-in for the LOD-DAG grouping step.
#[derive(Clone, Debug, Default)]
pub struct ClusterGraph {
    /// `adjacency[c]` = `(neighbour cluster, shared edge count)` pairs.
    pub adjacency: Vec<Vec<(u32, u32)>>,
}

/// Build the cluster adjacency graph: clusters sharing a boundary edge are
/// adjacent, weighted by how many edges they share. Higher weight ⇒ more shared
/// boundary ⇒ better to group together (the boundary becomes internal and can be
/// simplified).
pub fn build_cluster_graph(meshlets: &[Meshlet], indices: &[u32]) -> ClusterGraph {
    let n = meshlets.len();
    let tri_count = indices.len() / 3;
    let mut tri_cluster = vec![u32::MAX; tri_count];
    for (ci, m) in meshlets.iter().enumerate() {
        for &t in &m.triangles {
            tri_cluster[t as usize] = ci as u32;
        }
    }

    // Edge → the distinct clusters whose triangles use it.
    let mut edge_clusters: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for t in 0..tri_count as u32 {
        let c = tri_cluster[t as usize];
        if c == u32::MAX {
            continue; // degenerate / unassigned
        }
        let [a, b, cc] = tri_verts(indices, t);
        for (u, v) in [(a, b), (b, cc), (cc, a)] {
            let list = edge_clusters.entry(edge_key(u, v)).or_default();
            if !list.contains(&c) {
                list.push(c);
            }
        }
    }

    // Shared-edge weight per cluster pair.
    let mut weights: HashMap<(u32, u32), u32> = HashMap::new();
    for clusters in edge_clusters.values() {
        for i in 0..clusters.len() {
            for j in (i + 1)..clusters.len() {
                let (a, b) = (clusters[i], clusters[j]);
                *weights.entry(edge_key(a, b)).or_insert(0) += 1;
            }
        }
    }

    let mut adjacency = vec![Vec::new(); n];
    for ((a, b), w) in weights {
        adjacency[a as usize].push((b, w));
        adjacency[b as usize].push((a, w));
    }
    ClusterGraph { adjacency }
}

/// Partition clusters into groups of about `target_size`, greedily maximising
/// intra-group shared boundary (so each group's *external* boundary — the part
/// that must stay locked during simplification — is small). Every cluster lands
/// in exactly one group; a cluster with no ungrouped neighbours forms (or stays
/// in) a smaller group. Returns the groups as lists of cluster indices.
pub fn group_clusters(graph: &ClusterGraph, target_size: usize) -> Vec<Vec<u32>> {
    let n = graph.adjacency.len();
    let target = target_size.max(1);
    let mut grouped = vec![false; n];
    let mut groups = Vec::new();

    for seed in 0..n as u32 {
        if grouped[seed as usize] {
            continue;
        }
        let mut group = vec![seed];
        grouped[seed as usize] = true;

        while group.len() < target {
            // Total shared-edge weight from the current group to each ungrouped
            // neighbour; pick the strongest (ties → lowest id).
            let mut cand: HashMap<u32, u32> = HashMap::new();
            for &g in &group {
                for &(nb, w) in &graph.adjacency[g as usize] {
                    if !grouped[nb as usize] {
                        *cand.entry(nb).or_insert(0) += w;
                    }
                }
            }
            let mut best = u32::MAX;
            let mut best_w = 0u32;
            for (nb, w) in cand {
                if w > best_w || (w == best_w && nb < best) {
                    best_w = w;
                    best = nb;
                }
            }
            if best == u32::MAX {
                break; // no connected ungrouped cluster left
            }
            grouped[best as usize] = true;
            group.push(best);
        }
        groups.push(group);
    }
    groups
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

    #[test]
    fn cluster_graph_is_symmetric_and_weighted() {
        let (pos, indices) = grid(12);
        let clusters = build_clusters(&pos, &indices, 16);
        assert!(clusters.len() > 1, "need multiple clusters to be adjacent");
        let graph = build_cluster_graph(&clusters, &indices);
        assert_eq!(graph.adjacency.len(), clusters.len());

        // Symmetric: a→b with weight w implies b→a with weight w.
        for (a, nbrs) in graph.adjacency.iter().enumerate() {
            for &(b, w) in nbrs {
                assert!(w > 0, "adjacency weight must be positive");
                let back = graph.adjacency[b as usize]
                    .iter()
                    .find(|(x, _)| *x == a as u32)
                    .map(|(_, w)| *w);
                assert_eq!(back, Some(w), "adjacency must be symmetric");
            }
        }
        // A connected grid partition: every cluster has at least one neighbour.
        assert!(graph.adjacency.iter().all(|n| !n.is_empty()));
    }

    #[test]
    fn grouping_covers_all_clusters_within_target() {
        let (pos, indices) = grid(16); // 512 tris
        let clusters = build_clusters(&pos, &indices, 16);
        let graph = build_cluster_graph(&clusters, &indices);
        let target = 4;
        let groups = group_clusters(&graph, target);

        let mut seen = vec![false; clusters.len()];
        for g in &groups {
            assert!(!g.is_empty());
            assert!(g.len() <= target, "group exceeds target size");
            for &c in g {
                assert!(!seen[c as usize], "cluster {c} in two groups");
                seen[c as usize] = true;
            }
        }
        assert!(seen.iter().all(|&s| s), "every cluster grouped");
        // Most groups should reach the target on a large connected mesh.
        let full = groups.iter().filter(|g| g.len() == target).count();
        assert!(full > 0, "expected some full-size groups, got {groups:?}");
    }

    #[test]
    fn groups_are_internally_connected() {
        // Each non-singleton group must be edge-connected through the cluster
        // graph (greedy growth only adds connected clusters).
        let (pos, indices) = grid(14);
        let clusters = build_clusters(&pos, &indices, 16);
        let graph = build_cluster_graph(&clusters, &indices);
        let groups = group_clusters(&graph, 4);
        for g in &groups {
            if g.len() < 2 {
                continue;
            }
            let set: std::collections::HashSet<u32> = g.iter().copied().collect();
            // BFS from g[0] within the group must reach all members.
            let mut stack = vec![g[0]];
            let mut reached = std::collections::HashSet::new();
            reached.insert(g[0]);
            while let Some(c) = stack.pop() {
                for &(nb, _) in &graph.adjacency[c as usize] {
                    if set.contains(&nb) && reached.insert(nb) {
                        stack.push(nb);
                    }
                }
            }
            assert_eq!(reached.len(), g.len(), "group must be connected");
        }
    }
}
