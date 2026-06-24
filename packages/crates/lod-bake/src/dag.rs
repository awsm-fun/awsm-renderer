//! Cluster LOD DAG build (Phase B, B.1c).
//!
//! Nanite-style: partition into clusters (level 0), then repeatedly group
//! adjacent clusters, **boundary-locked-simplify** each group to ~half, re-split
//! the result into coarser clusters, and record a monotonic per-cluster error —
//! until a single root remains (or no further reduction is possible).
//!
//! Two properties make this fall out of the existing primitives:
//! - **Subset vertices.** The half-edge collapse keeps survivors a subset of the
//!   originals, so *every* cluster at *every* level indexes the same original
//!   vertex buffer — no new vertices, no cross-level vertex bookkeeping.
//! - **Crack-free for free.** Simplifying a group's triangles *in isolation*
//!   makes its external-boundary edges one-sided (used by a single group
//!   triangle) → the simplifier locks them. Adjacent groups share that boundary
//!   and lock it identically, so seams never crack. Group-internal edges (two
//!   group triangles) collapse normally.

use std::collections::HashMap;

use glam::DVec3;

use crate::cluster::{build_cluster_graph, build_clusters, group_clusters, Meshlet};
use crate::simplify::{simplify, SimplifyOptions};

/// `parent_error` for a root cluster (never simplified further). A large finite
/// sentinel rather than `f32::INFINITY` so the bake output is plain-text
/// serialisable (JSON has no infinity); the cut test `threshold < parent_error`
/// is satisfied for every realistic threshold, exactly as infinity would be.
pub const ROOT_PARENT_ERROR: f32 = f32::MAX;

/// One cluster in the DAG. Triangles index the shared original vertex buffer.
#[derive(Clone, Debug)]
pub struct DagCluster {
    /// Triangles as original-vertex-index triples.
    pub triangles: Vec<[u32; 3]>,
    /// Bounding-sphere centre + radius (object space).
    pub center: [f32; 3],
    pub radius: f32,
    /// The error introduced when this cluster was created by simplifying its
    /// source group (`0` for level-0 clusters — taken verbatim from the input).
    pub lod_error: f32,
    /// The error of the group that simplifies THIS cluster away into a coarser
    /// parent. [`ROOT_PARENT_ERROR`] for root clusters (never simplified
    /// further). Monotonic: `parent_error >= lod_error`. Runtime LOD cut: render
    /// a cluster when `lod_error <= threshold < parent_error`.
    pub parent_error: f32,
    /// Bounding sphere of the **group that created this cluster** (the source
    /// group simplified into it) — the sphere a GPU per-cluster cut projects
    /// `lod_error` against. Group-shared, so every cluster of a group flips at
    /// the same camera threshold ⇒ crack-free. Equals the cluster's own bounds
    /// for level-0 clusters (no creating group).
    pub lod_bounds_center: [f32; 3],
    pub lod_bounds_radius: f32,
    /// Bounding sphere of the **group that simplifies this cluster away** — the
    /// sphere a GPU per-cluster cut projects `parent_error` against. Equals the
    /// cluster's own bounds for roots (never simplified). A child's
    /// `parent_bounds` is exactly the `lod_bounds` of the coarser clusters its
    /// group produced — the shared sphere that keeps the cut watertight.
    pub parent_bounds_center: [f32; 3],
    pub parent_bounds_radius: f32,
}

/// The full DAG: clusters across all LOD levels, all indexing one vertex buffer.
#[derive(Clone, Debug, Default)]
pub struct ClusterDag {
    pub clusters: Vec<DagCluster>,
}

/// Knobs for [`build_cluster_dag`].
#[derive(Clone, Copy, Debug)]
pub struct DagOptions {
    /// Target triangles per cluster (~128 for Nanite-style meshlets).
    pub cluster_target: usize,
    /// Clusters per group for the simplify step (~4–8).
    pub group_size: usize,
    /// Target triangle fraction when simplifying a group (~0.5).
    pub simplify_ratio: f32,
    /// Safety cap on DAG levels.
    pub max_levels: usize,
}

impl Default for DagOptions {
    fn default() -> Self {
        Self {
            cluster_target: 128,
            group_size: 4,
            simplify_ratio: 0.5,
            max_levels: 32,
        }
    }
}

/// Build the cluster LOD DAG for `(positions, indices)`.
pub fn build_cluster_dag(
    positions: &[[f32; 3]],
    indices: &[u32],
    opts: &DagOptions,
) -> ClusterDag {
    let pos: Vec<DVec3> = positions
        .iter()
        .map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))
        .collect();

    let mut clusters: Vec<DagCluster> = Vec::new();

    // Level 0: clusters straight from the input, zero error.
    let mut current: Vec<usize> = Vec::new();
    for m in build_clusters(positions, indices, opts.cluster_target) {
        let tris: Vec<[u32; 3]> = m
            .triangles
            .iter()
            .map(|&t| {
                let i = t as usize * 3;
                [indices[i], indices[i + 1], indices[i + 2]]
            })
            .collect();
        clusters.push(make_cluster(tris, &pos, 0.0));
        current.push(clusters.len() - 1);
    }

    let mut level = 0;
    while current.len() > 1 && level < opts.max_levels {
        level += 1;
        let cur_tris: usize = current.iter().map(|&c| clusters[c].triangles.len()).sum();

        // Treat the current clusters as a meshlet set over a combined index
        // buffer so the adjacency/grouping primitives apply.
        let mut combined: Vec<u32> = Vec::new();
        let mut level_meshlets: Vec<Meshlet> = Vec::with_capacity(current.len());
        for &ci in &current {
            let start = combined.len() / 3;
            for tri in &clusters[ci].triangles {
                combined.extend_from_slice(tri);
            }
            let tri_ids: Vec<u32> = (start as u32..(combined.len() / 3) as u32).collect();
            level_meshlets.push(Meshlet {
                triangles: tri_ids,
                center: clusters[ci].center,
                radius: clusters[ci].radius,
            });
        }
        let graph = build_cluster_graph(&level_meshlets, &combined);
        let groups = group_clusters(&graph, opts.group_size);

        let mut next: Vec<usize> = Vec::new();
        let mut next_tris = 0usize;
        for group in &groups {
            // Merge the group's triangles into one submesh (original indices).
            let mut group_indices: Vec<u32> = Vec::new();
            let mut max_child_error = 0.0f32;
            for &local in group {
                let ci = current[local as usize];
                for tri in &clusters[ci].triangles {
                    group_indices.extend_from_slice(tri);
                }
                max_child_error = max_child_error.max(clusters[ci].lod_error);
            }

            // Simplify the group in isolation (→ crack-free boundary lock),
            // compacting to the group's own vertices first so the simplifier's
            // arrays are group-sized, not whole-mesh-sized (scale).
            let (local_pos, local_idx, local_to_orig) = compact_submesh(positions, &group_indices);
            let group_tris = local_idx.len() / 3;
            let target = ((group_tris as f32 * opts.simplify_ratio).round() as usize).max(1);
            let sm = simplify(&local_pos, &local_idx, SimplifyOptions::with_target(target));

            // Group sphere: the shared bounds all the group's clusters project
            // their flip threshold against, so they switch together (crack-free).
            let (group_c, group_r) = sphere_of(&group_indices, &pos);

            // Monotonic group error: at least any child's, plus this collapse's.
            let group_error = sm.error.max(max_child_error) + f32::EPSILON;
            for &local in group {
                let ci = current[local as usize];
                clusters[ci].parent_error = group_error;
                clusters[ci].parent_bounds_center = group_c;
                clusters[ci].parent_bounds_radius = group_r;
            }

            // Reconstruct simplified triangles in ORIGINAL vertex indices.
            let simplified: Vec<[u32; 3]> = sm
                .indices
                .chunks_exact(3)
                .map(|c| {
                    [
                        local_to_orig[sm.surviving[c[0] as usize] as usize],
                        local_to_orig[sm.surviving[c[1] as usize] as usize],
                        local_to_orig[sm.surviving[c[2] as usize] as usize],
                    ]
                })
                .collect();

            // Re-split the simplified group into coarser clusters.
            let flat: Vec<u32> = simplified.iter().flatten().copied().collect();
            for nm in build_clusters(positions, &flat, opts.cluster_target) {
                let tris: Vec<[u32; 3]> = nm
                    .triangles
                    .iter()
                    .map(|&t| {
                        let i = t as usize * 3;
                        [flat[i], flat[i + 1], flat[i + 2]]
                    })
                    .collect();
                next_tris += tris.len();
                let mut nc = make_cluster(tris, &pos, group_error);
                // This cluster was created by simplifying `group`; project its
                // own `lod_error` against the shared group sphere.
                nc.lod_bounds_center = group_c;
                nc.lod_bounds_radius = group_r;
                clusters.push(nc);
                next.push(clusters.len() - 1);
            }
        }

        // Stop when a level can't reduce (boundary-locked geometry at its floor);
        // the current clusters are then the effective roots.
        if next.is_empty() || next_tris >= cur_tris {
            break;
        }
        current = next;
    }

    ClusterDag { clusters }
}

/// Compact a submesh (triangles in original indices) to a dense local vertex
/// space. Returns `(local_positions, local_indices, local→original)`.
fn compact_submesh(
    positions: &[[f32; 3]],
    indices: &[u32],
) -> (Vec<[f32; 3]>, Vec<u32>, Vec<u32>) {
    let mut orig_to_local: HashMap<u32, u32> = HashMap::new();
    let mut local_to_orig: Vec<u32> = Vec::new();
    let mut local_pos: Vec<[f32; 3]> = Vec::new();
    let mut local_idx: Vec<u32> = Vec::with_capacity(indices.len());
    for &v in indices {
        let l = *orig_to_local.entry(v).or_insert_with(|| {
            local_to_orig.push(v);
            local_pos.push(positions[v as usize]);
            (local_to_orig.len() - 1) as u32
        });
        local_idx.push(l);
    }
    (local_pos, local_idx, local_to_orig)
}

fn make_cluster(triangles: Vec<[u32; 3]>, pos: &[DVec3], lod_error: f32) -> DagCluster {
    let mut center = DVec3::ZERO;
    let mut n = 0u32;
    let mut seen = std::collections::HashSet::new();
    for tri in &triangles {
        for &v in tri {
            if seen.insert(v) {
                center += pos[v as usize];
                n += 1;
            }
        }
    }
    if n > 0 {
        center /= n as f64;
    }
    let mut r2 = 0.0_f64;
    for &v in &seen {
        r2 = r2.max((pos[v as usize] - center).length_squared());
    }
    let c = [center.x as f32, center.y as f32, center.z as f32];
    let r = r2.sqrt() as f32;
    DagCluster {
        triangles,
        center: c,
        radius: r,
        lod_error,
        parent_error: ROOT_PARENT_ERROR,
        // Default to own bounds; the DAG build overwrites with the group sphere
        // (lod_bounds for clusters a group creates, parent_bounds for a group's
        // children). Level-0 clusters keep own bounds for lod; roots for parent.
        lod_bounds_center: c,
        lod_bounds_radius: r,
        parent_bounds_center: c,
        parent_bounds_radius: r,
    }
}

/// Bounding sphere (centre, radius) over the vertices referenced by `indices`.
fn sphere_of(indices: &[u32], pos: &[DVec3]) -> ([f32; 3], f32) {
    let mut verts = std::collections::HashSet::new();
    for &v in indices {
        verts.insert(v);
    }
    let mut c = DVec3::ZERO;
    for &v in &verts {
        c += pos[v as usize];
    }
    if !verts.is_empty() {
        c /= verts.len() as f64;
    }
    let mut r2 = 0.0_f64;
    for &v in &verts {
        r2 = r2.max((pos[v as usize] - c).length_squared());
    }
    ([c.x as f32, c.y as f32, c.z as f32], r2.sqrt() as f32)
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

    #[test]
    fn dag_builds_levels_and_is_monotone() {
        let (pos, indices) = grid(24); // 1152 tris
        let opts = DagOptions {
            cluster_target: 64,
            group_size: 4,
            simplify_ratio: 0.5,
            max_levels: 16,
        };
        let dag = build_cluster_dag(&pos, &indices, &opts);
        assert!(!dag.clusters.is_empty());

        let mut level0 = 0;
        let mut coarser = 0;
        for c in &dag.clusters {
            // Monotonic error: parent always at least this cluster's own error.
            assert!(
                c.parent_error >= c.lod_error,
                "parent_error {} < lod_error {}",
                c.parent_error,
                c.lod_error
            );
            assert!(c.radius > 0.0);
            assert!(!c.triangles.is_empty());
            // All indices reference real vertices.
            for tri in &c.triangles {
                for &v in tri {
                    assert!((v as usize) < pos.len());
                }
            }
            if c.lod_error == 0.0 {
                level0 += 1;
            } else {
                coarser += 1;
            }
        }
        assert!(level0 > 0, "must have level-0 clusters");
        assert!(coarser > 0, "DAG must build at least one coarser level");
    }

    #[test]
    fn level0_covers_every_triangle_once() {
        let (pos, indices) = grid(16);
        let total = indices.len() / 3;
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        // Level-0 clusters (error 0) partition the original triangles.
        let l0_tris: usize = dag
            .clusters
            .iter()
            .filter(|c| c.lod_error == 0.0)
            .map(|c| c.triangles.len())
            .sum();
        assert_eq!(l0_tris, total);
    }

    #[test]
    fn roots_have_root_parent_error() {
        let (pos, indices) = grid(20);
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        // At least one cluster is a root (never simplified further).
        assert!(dag.clusters.iter().any(|c| c.parent_error == ROOT_PARENT_ERROR));
    }

    #[test]
    fn group_bounds_are_valid_and_shared() {
        let (pos, indices) = grid(24);
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        // Every cluster has finite, non-negative group bounds.
        for c in &dag.clusters {
            assert!(c.lod_bounds_radius >= 0.0 && c.lod_bounds_radius.is_finite());
            assert!(c.parent_bounds_radius >= 0.0 && c.parent_bounds_radius.is_finite());
        }
        // A non-root cluster's vertices all lie within its parent (group) sphere
        // — the cluster belongs to that group, so it shares the sphere all the
        // group's clusters flip against (the crack-free invariant).
        for c in &dag.clusters {
            if c.parent_error >= ROOT_PARENT_ERROR {
                continue; // root: parent bounds default to own
            }
            let ctr = c.parent_bounds_center;
            for tri in &c.triangles {
                for &v in tri {
                    let p = pos[v as usize];
                    let d = ((p[0] - ctr[0]).powi(2)
                        + (p[1] - ctr[1]).powi(2)
                        + (p[2] - ctr[2]).powi(2))
                    .sqrt();
                    assert!(
                        d <= c.parent_bounds_radius + 1e-3,
                        "cluster vertex must lie within its parent (group) sphere"
                    );
                }
            }
        }
        // Coarser clusters (lod_error > 0) carry a group lod sphere distinct from
        // a degenerate point.
        assert!(dag
            .clusters
            .iter()
            .any(|c| c.lod_error > 0.0 && c.lod_bounds_radius > 0.0));
    }

    #[test]
    fn tiny_mesh_terminates() {
        let (pos, indices) = grid(2); // 8 tris → one cluster, no further levels
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        assert!(!dag.clusters.is_empty());
        assert!(dag
            .clusters
            .iter()
            .all(|c| c.parent_error == ROOT_PARENT_ERROR));
    }
}
