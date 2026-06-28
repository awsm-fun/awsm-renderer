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

use crate::cluster::{build_cluster_graph, build_clusters_welded, group_clusters, Meshlet};
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
    /// Position-weld epsilon for ADJACENCY only. Triangles that share a geometric
    /// edge but reference distinct vertex indices (UV/normal seams — extremely
    /// common in imported glTF) are otherwise seen as disconnected, so clustering
    /// degenerates to ~1 triangle per cluster. Welding coincident positions onto an
    /// `eps` grid restores adjacency so triangles group properly. `None` disables
    /// welding (raw-index adjacency). Output triangle indices are ALWAYS the
    /// originals — welding never merges vertices in the geometry, so split
    /// normals/uvs survive. Default `Some(1e-5)`.
    pub weld_eps: Option<f32>,
}

impl Default for DagOptions {
    fn default() -> Self {
        Self {
            cluster_target: 128,
            group_size: 4,
            simplify_ratio: 0.5,
            max_levels: 32,
            weld_eps: Some(1e-5),
        }
    }
}

/// Weld positions onto an `eps` grid → a canonical vertex id per location, so
/// coincident-but-distinct seam/pole vertices unify. One welded id per input
/// vertex. Used for ADJACENCY ONLY (see [`DagOptions::weld_eps`]).
pub(crate) fn weld_ids(positions: &[[f32; 3]], eps: f32) -> Vec<u32> {
    use std::collections::HashMap;
    let mut map: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let q = |v: f32| (v / eps).round() as i64;
    positions
        .iter()
        .map(|p| {
            let key = (q(p[0]), q(p[1]), q(p[2]));
            let n = map.len() as u32;
            *map.entry(key).or_insert(n)
        })
        .collect()
}

/// Build the cluster LOD DAG for `(positions, indices)`.
pub fn build_cluster_dag(positions: &[[f32; 3]], indices: &[u32], opts: &DagOptions) -> ClusterDag {
    let pos: Vec<DVec3> = positions
        .iter()
        .map(|p| DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64))
        .collect();

    let mut clusters: Vec<DagCluster> = Vec::new();

    // Weld coincident positions for ADJACENCY ONLY (seam/pole split verts —
    // ubiquitous in imported glTF — otherwise break edge adjacency and clustering
    // degenerates to ~1 tri/cluster). Output triangles always use the ORIGINAL
    // indices, so split normals/uvs survive. `welded(&buf)` relabels an index
    // buffer through the weld map (identity when welding is off).
    let weld = opts.weld_eps.map(|eps| weld_ids(positions, eps));

    // Level 0: clusters straight from the input, zero error.
    let mut current: Vec<usize> = Vec::new();
    for m in build_clusters_welded(positions, indices, opts.cluster_target, weld.as_deref()) {
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
                // Welded ids for the graph's edge adjacency (output unaffected).
                match &weld {
                    Some(w) => combined.extend(tri.iter().map(|&v| w[v as usize])),
                    None => combined.extend_from_slice(tri),
                }
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
            let sm = simplify(
                &local_pos,
                &local_idx,
                SimplifyOptions::with_target_locked(target),
            );

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
            for nm in build_clusters_welded(positions, &flat, opts.cluster_target, weld.as_deref())
            {
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
fn compact_submesh(positions: &[[f32; 3]], indices: &[u32]) -> (Vec<[f32; 3]>, Vec<u32>, Vec<u32>) {
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

    /// A lat/long UV sphere — **geometrically closed** but **non-watertight by
    /// index**: the longitude seam column is duplicated (phi=0 vs phi=TAU are
    /// coincident positions with distinct indices) and the pole rows are coincident
    /// duplicates. This mirrors `meshgen::primitives::sphere_mesh` exactly (the
    /// shape the editor's "sphere + Subdivide" repro feeds the bake). Index-based
    /// adjacency therefore sees the seam as an open boundary, which is the
    /// non-watertight input class the cluster bake must stay crack-free on (A1).
    fn uv_sphere(segments_long: usize, segments_lat: usize) -> (Vec<[f32; 3]>, Vec<u32>) {
        use std::f32::consts::PI;
        const TAU: f32 = 2.0 * PI;
        let mut pos = Vec::new();
        for lat in 0..=segments_lat {
            let theta = (lat as f32 / segments_lat as f32) * PI;
            let (sin_t, cos_t) = (theta.sin(), theta.cos());
            for lon in 0..=segments_long {
                let phi = (lon as f32 / segments_long as f32) * TAU;
                let (sin_p, cos_p) = (phi.sin(), phi.cos());
                pos.push([sin_t * cos_p, cos_t, sin_t * sin_p]);
            }
        }
        let stride = segments_long + 1;
        let mut indices = Vec::new();
        for lat in 0..segments_lat {
            for lon in 0..segments_long {
                let a = (lat * stride + lon) as u32;
                let b = (lat * stride + lon + 1) as u32;
                let c = ((lat + 1) * stride + lon + 1) as u32;
                let d = ((lat + 1) * stride + lon) as u32;
                indices.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }
        (pos, indices)
    }

    /// A UV torus (major radius 1, minor 0.35) — like [`uv_sphere`], geometrically
    /// **closed** but **non-watertight by index** (both the major and minor seam
    /// rings are duplicated columns/rows). Genus-1, so a topologically distinct
    /// crack-free fixture from the sphere: the cut must stay closed on it too.
    fn uv_torus(segments_major: usize, segments_minor: usize) -> (Vec<[f32; 3]>, Vec<u32>) {
        use std::f32::consts::PI;
        const TAU: f32 = 2.0 * PI;
        let (big_r, small_r) = (1.0f32, 0.35f32);
        let mut pos = Vec::new();
        for i in 0..=segments_major {
            let theta = (i as f32 / segments_major as f32) * TAU;
            let (sin_t, cos_t) = (theta.sin(), theta.cos());
            for j in 0..=segments_minor {
                let phi = (j as f32 / segments_minor as f32) * TAU;
                let (sin_p, cos_p) = (phi.sin(), phi.cos());
                let ring = big_r + small_r * cos_p;
                pos.push([ring * cos_t, ring * sin_t, small_r * sin_p]);
            }
        }
        let stride = segments_minor + 1;
        let mut indices = Vec::new();
        for i in 0..segments_major {
            for j in 0..segments_minor {
                let a = (i * stride + j) as u32;
                let b = (i * stride + j + 1) as u32;
                let c = ((i + 1) * stride + j + 1) as u32;
                let d = ((i + 1) * stride + j) as u32;
                indices.extend_from_slice(&[a, b, c, a, c, d]);
            }
        }
        (pos, indices)
    }

    /// Weld positions onto an epsilon grid → a canonical vertex id per location,
    /// so coincident-but-distinct seam/pole vertices unify. Returns one welded id
    /// per input vertex index.
    fn weld_ids(pos: &[[f32; 3]], eps: f32) -> Vec<u32> {
        use std::collections::HashMap;
        let mut map: HashMap<(i64, i64, i64), u32> = HashMap::new();
        let q = |v: f32| (v / eps).round() as i64;
        pos.iter()
            .map(|p| {
                let key = (q(p[0]), q(p[1]), q(p[2]));
                let n = map.len() as u32;
                *map.entry(key).or_insert(n)
            })
            .collect()
    }

    /// Count undirected edges used by exactly one (welded, non-degenerate)
    /// triangle — i.e. **hole / boundary edges**. Zero ⇒ the surface is closed
    /// (crack-free); any open edge on a closed source is a torn hole.
    fn boundary_edge_count(tris: &[[u32; 3]], weld: &[u32]) -> usize {
        use std::collections::HashMap;
        let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
        for tri in tris {
            let w = [
                weld[tri[0] as usize],
                weld[tri[1] as usize],
                weld[tri[2] as usize],
            ];
            // Drop triangles that became degenerate after welding (e.g. pole
            // slivers whose two pole verts unify) — they are not real surface.
            if w[0] == w[1] || w[1] == w[2] || w[0] == w[2] {
                continue;
            }
            for (a, b) in [(w[0], w[1]), (w[1], w[2]), (w[2], w[0])] {
                let key = if a < b { (a, b) } else { (b, a) };
                *edges.entry(key).or_insert(0) += 1;
            }
        }
        edges.values().filter(|&&c| c == 1).count()
    }

    /// Simulate the runtime per-cluster cut at a scalar error `threshold`: select
    /// each cluster whose error interval `[lod_error, parent_error)` contains it
    /// (exactly the GPU rule, minus the screen-space projection). Returns the
    /// selected clusters' triangles.
    fn cut_triangles(dag: &ClusterDag, threshold: f32) -> Vec<[u32; 3]> {
        let mut out = Vec::new();
        for c in &dag.clusters {
            if c.lod_error <= threshold && threshold < c.parent_error {
                out.extend_from_slice(&c.triangles);
            }
        }
        out
    }

    /// A1 (north-star): a geometrically-closed but **non-watertight-by-index**
    /// mesh (UV sphere: duplicated seam column + pole rows) must stay **crack-free
    /// at every LOD level**. Cut at any error threshold, the selected antichain —
    /// welded by position — must be a closed surface (no hole edges). A torn coarse
    /// level (the reported subdivided-sphere holes) shows up here as open edges.
    ///
    /// Was the Gap-A reproduction (the first coarse level tore ~21 hole edges
    /// because index-based adjacency treated the coincident seam/pole duplicates
    /// as open boundaries). Fixed by position-welding the simplifier's topology
    /// (`simplify::weld_coincident`).
    #[test]
    fn non_watertight_sphere_cut_is_closed_at_every_level() {
        let (pos, indices) = uv_sphere(48, 32); // 48*32*2 = 3072 tris, multi-level DAG
        let eps = 1e-3;
        let weld = weld_ids(&pos, eps);

        // Sanity: the SOURCE, welded, is a closed manifold (no hole edges).
        let src_tris: Vec<[u32; 3]> = indices
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        assert_eq!(
            boundary_edge_count(&src_tris, &weld),
            0,
            "source UV sphere must be geometrically closed once welded"
        );

        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());

        // Level-0 reconstructs the source triangles exactly (no dropped coverage).
        let l0: usize = dag
            .clusters
            .iter()
            .filter(|c| c.lod_error == 0.0)
            .map(|c| c.triangles.len())
            .sum();
        assert_eq!(
            l0,
            indices.len() / 3,
            "level-0 must cover every source triangle"
        );

        // Sweep every error breakpoint in the DAG (each cluster's lod_error, plus
        // just above it) — the cut must be a closed surface at all of them.
        let mut breakpoints: Vec<f32> = vec![0.0];
        for c in &dag.clusters {
            breakpoints.push(c.lod_error);
            if c.parent_error < ROOT_PARENT_ERROR {
                breakpoints.push((c.lod_error + c.parent_error) * 0.5);
            }
        }
        breakpoints.sort_by(|a, b| a.partial_cmp(b).unwrap());
        breakpoints.dedup();

        for &t in &breakpoints {
            let tris = cut_triangles(&dag, t);
            assert!(!tris.is_empty(), "cut at threshold {t} selected nothing");
            let holes = boundary_edge_count(&tris, &weld);
            assert_eq!(
                holes, 0,
                "cluster cut at error threshold {t} tore {holes} hole edge(s) — \
                 not crack-free on non-watertight (seam/pole) input (A1)"
            );
        }

        // The fix must not "succeed" by refusing to simplify: the coarsest cut
        // (just below the largest cluster error) must be a real LOD reduction.
        let max_err = dag
            .clusters
            .iter()
            .filter(|c| c.parent_error < ROOT_PARENT_ERROR)
            .map(|c| c.lod_error)
            .fold(0.0f32, f32::max);
        let coarsest = cut_triangles(&dag, max_err).len();
        let source = indices.len() / 3;
        assert!(
            coarsest < source * 3 / 4,
            "coarsest cut {coarsest} is not a real reduction from {source} \
             (locked-boundary simplify must still coarsen the sphere)"
        );
    }

    /// A1, extended to a genus-1 surface: a non-watertight-by-index torus must
    /// also stay crack-free at every LOD level. Same invariant as the sphere test,
    /// different topology — guards against a fix that accidentally relied on
    /// sphere-specific structure.
    #[test]
    fn non_watertight_torus_cut_is_closed_at_every_level() {
        let (pos, indices) = uv_torus(48, 24); // 48*24*2 = 2304 tris, multi-level DAG
        let weld = weld_ids(&pos, 1e-3);

        let src_tris: Vec<[u32; 3]> = indices
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        assert_eq!(
            boundary_edge_count(&src_tris, &weld),
            0,
            "source UV torus must be geometrically closed once welded"
        );

        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());

        let mut breakpoints: Vec<f32> = vec![0.0];
        for c in &dag.clusters {
            breakpoints.push(c.lod_error);
            if c.parent_error < ROOT_PARENT_ERROR {
                breakpoints.push((c.lod_error + c.parent_error) * 0.5);
            }
        }
        breakpoints.sort_by(|a, b| a.partial_cmp(b).unwrap());
        breakpoints.dedup();

        for &t in &breakpoints {
            let tris = cut_triangles(&dag, t);
            assert!(
                !tris.is_empty(),
                "torus cut at threshold {t} selected nothing"
            );
            let holes = boundary_edge_count(&tris, &weld);
            assert_eq!(
                holes, 0,
                "torus cluster cut at error threshold {t} tore {holes} hole edge(s) — \
                 not crack-free on non-watertight genus-1 input (A1)"
            );
        }

        let max_err = dag
            .clusters
            .iter()
            .filter(|c| c.parent_error < ROOT_PARENT_ERROR)
            .map(|c| c.lod_error)
            .fold(0.0f32, f32::max);
        let coarsest = cut_triangles(&dag, max_err).len();
        let source = indices.len() / 3;
        assert!(
            coarsest < source * 3 / 4,
            "coarsest torus cut {coarsest} is not a real reduction from {source}"
        );
    }

    /// A "triangle soup" — every triangle at a distinct location, sharing no edges
    /// or positions — is genuinely **unweldable**: position-welding can't restore
    /// adjacency because nothing is coincident, so clustering collapses to ~1
    /// tri/cluster even with the DEFAULT (welding-on) options. `ClusterMesh::quality`
    /// must flag it degenerate, which is exactly what makes the editor bake drop the
    /// cluster DAG and fall back to the discrete LOD chain (B2).
    #[test]
    fn triangle_soup_is_flagged_degenerate() {
        use crate::cluster_mesh::ClusterMesh;
        let mut pos = Vec::new();
        let mut indices = Vec::new();
        // 600 isolated triangles spread far apart so no two share a position.
        for k in 0..600u32 {
            let base = pos.len() as u32;
            let off = (k as f32) * 10.0;
            pos.push([off, 0.0, 0.0]);
            pos.push([off + 1.0, 0.0, 0.0]);
            pos.push([off, 1.0, 0.0]);
            indices.extend_from_slice(&[base, base + 1, base + 2]);
        }
        let source_tris = indices.len() / 3;
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        let cm = ClusterMesh::from_dag(&dag, pos, vec![], vec![], vec![]);
        let q = cm.quality(source_tris);
        assert!(
            q.degenerate,
            "unweldable triangle soup not flagged: {:.1} tris/cluster, {:.1}× source",
            q.avg_tris_per_cluster, q.dag_ratio
        );
    }

    #[test]
    fn dag_builds_levels_and_is_monotone() {
        let (pos, indices) = grid(24); // 1152 tris
        let opts = DagOptions {
            cluster_target: 64,
            group_size: 4,
            simplify_ratio: 0.5,
            max_levels: 16,
            weld_eps: Some(1e-5),
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

    /// Imported glTF commonly splits vertices at UV/normal seams, so triangles
    /// that share a geometric edge reference DISTINCT indices. Without
    /// weld-for-adjacency the clusterer sees them as disconnected and degenerates
    /// to ~1 triangle per cluster (and the DAG explodes). `weld_eps` (default on)
    /// must restore adjacency: far fewer clusters, far healthier tris/cluster.
    #[test]
    fn weld_adjacency_fixes_split_vertex_degeneracy() {
        // A welded grid, then fully un-welded (every triangle gets its own 3 verts
        // → zero shared indices), keeping identical positions.
        let (pos, indices) = grid(24); // 1152 tris, shared verts
        let tris = indices.len() / 3;
        let mut split_pos: Vec<[f32; 3]> = Vec::with_capacity(indices.len());
        let mut split_idx: Vec<u32> = Vec::with_capacity(indices.len());
        for (k, &i) in indices.iter().enumerate() {
            split_pos.push(pos[i as usize]);
            split_idx.push(k as u32);
        }

        let off = DagOptions {
            weld_eps: None,
            ..DagOptions::default()
        };
        let on = DagOptions::default(); // weld_eps: Some(1e-5)

        let degen = build_cluster_dag(&split_pos, &split_idx, &off);
        let welded = build_cluster_dag(&split_pos, &split_idx, &on);
        let baseline = build_cluster_dag(&pos, &indices, &on); // already-shared grid

        let l0 = |d: &ClusterDag| d.clusters.iter().filter(|c| c.lod_error == 0.0).count();
        let (degen_l0, welded_l0, base_l0) = (l0(&degen), l0(&welded), l0(&baseline));

        // Welding the split mesh recovers ~the same level-0 cluster count as the
        // already-shared grid, and is dramatically better than raw-index adjacency.
        assert!(
            welded_l0 * 4 < degen_l0,
            "weld should cut level-0 clusters ≥4× (split-vert degeneracy): \
             welded={welded_l0} vs raw={degen_l0}"
        );
        assert!(
            welded_l0 <= base_l0 * 2,
            "welded split mesh ({welded_l0}) should cluster ~like the shared grid ({base_l0})"
        );
        // Healthy average: welded ≥ 16 tris/cluster; degenerate ≈ 1.
        assert!(
            tris / welded_l0.max(1) >= 16,
            "welded avg tris/cluster too low: {} ({tris} tris / {welded_l0} clusters)",
            tris / welded_l0.max(1)
        );
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
        assert!(dag
            .clusters
            .iter()
            .any(|c| c.parent_error == ROOT_PARENT_ERROR));
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

    /// Scale smoke test: the bake must stay roughly linear, not blow up, on a
    /// large mesh (the plan's multi-million-tri requirement, proxied here by a
    /// dense grid that keeps the unit test fast). A super-linear regression in
    /// clustering / grouping / per-group simplify would time this out.
    #[test]
    fn large_mesh_builds_a_valid_dag() {
        let (pos, indices) = grid(200); // 200×200×2 = 80,000 triangles
        let total = indices.len() / 3;
        assert_eq!(total, 80_000);
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());

        // Level-0 clusters partition the source triangles exactly.
        let l0: usize = dag
            .clusters
            .iter()
            .filter(|c| c.lod_error == 0.0)
            .map(|c| c.triangles.len())
            .sum();
        assert_eq!(l0, total, "level 0 must cover every source triangle");
        // The DAG reduced to coarser levels and a root exists.
        assert!(
            dag.clusters.iter().any(|c| c.lod_error > 0.0),
            "built coarser levels"
        );
        assert!(
            dag.clusters
                .iter()
                .any(|c| c.parent_error >= ROOT_PARENT_ERROR),
            "has a root"
        );
        // Monotonic errors + valid bounds everywhere.
        for c in &dag.clusters {
            assert!(c.parent_error >= c.lod_error);
            assert!(c.radius > 0.0 && c.radius.is_finite());
        }
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
