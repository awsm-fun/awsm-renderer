//! Serializable cluster-LOD bake output (Phase B, B.1d): a shared vertex buffer
//! plus per-cluster **index pages** and meta, ready to upload as GPU buffers and
//! evaluate the LOD cut on-device (B.2/B.3).
//!
//! Indexed, not yet exploded: every cluster's triangles index the one shared
//! vertex buffer (the DAG's subset-vertex property), and each cluster occupies a
//! contiguous `[first_index, first_index+index_count)` slice of `indices`. The
//! renderer explodes these into its 56-byte visibility-vertex layout at upload.
//!
//! The bundle serialises this as **binary** (e.g. bincode) — `parent_error`
//! carries `f32::INFINITY` for roots, which text formats can't represent.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::dag::ClusterDag;

/// Minimum healthy average triangles-per-cluster. A well-clustered DAG averages
/// dozens (the cluster target is ~128); pathological source topology (non-manifold
/// or unweldable split-vertices) that defeats clustering even after the
/// weld-for-adjacency pass collapses to ~1 tri/cluster. Below this we call the bake
/// degenerate. See [`ClusterMesh::quality`].
pub const MIN_AVG_TRIS_PER_CLUSTER: f32 = 8.0;

/// Maximum healthy DAG-triangle / source-triangle ratio. A healthy DAG totals ~2×
/// the source (each coarser level roughly halves), so well under this. A degenerate
/// clustering balloons many× the source instead of coarsening — a huge, useless
/// `.clusters.bin` that also tends to cut with holes. See [`ClusterMesh::quality`].
pub const MAX_DAG_TRI_RATIO: f32 = 6.0;

/// Quality metrics for a baked cluster DAG, with the degeneracy verdict
/// ([`ClusterMesh::quality`]). The SINGLE source of truth for "is this DAG worth
/// shipping" — shared by the offline CLI bake (`lod-bake-cli`) and the editor's
/// export-time bake (`controller::lod_bake::bake_static_clusters`) so the two can't
/// drift. A degenerate DAG should be dropped (the discrete LOD chain still ships).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DagQuality {
    pub cluster_count: usize,
    pub dag_triangles: usize,
    /// `dag_triangles / cluster_count` — low ⇒ clustering failed.
    pub avg_tris_per_cluster: f32,
    /// `dag_triangles / source_triangles` — high ⇒ DAG ballooned instead of coarsening.
    pub dag_ratio: f32,
    /// `avg_tris_per_cluster < MIN_AVG_TRIS_PER_CLUSTER || dag_ratio > MAX_DAG_TRI_RATIO`.
    pub degenerate: bool,
}

/// One cluster's page: its bounds, LOD errors, and where its indices live.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClusterPage {
    /// Bounding-sphere centre (object space).
    pub center: [f32; 3],
    /// Bounding-sphere radius.
    pub radius: f32,
    /// Error introduced creating this cluster (`0` at the finest level).
    pub lod_error: f32,
    /// Error of the group that simplifies this cluster away (root sentinel for
    /// roots). LOD cut: draw when `lod_error <= threshold < parent_error`.
    pub parent_error: f32,
    /// Group sphere to project `lod_error` against (group-shared ⇒ crack-free
    /// per-cluster cut). See [`crate::dag::DagCluster::lod_bounds_center`].
    pub lod_bounds_center: [f32; 3],
    pub lod_bounds_radius: f32,
    /// Group sphere to project `parent_error` against.
    pub parent_bounds_center: [f32; 3],
    pub parent_bounds_radius: f32,
    /// First index of this cluster's triangles in [`ClusterMesh::indices`].
    pub first_index: u32,
    /// Number of indices (triangle count × 3).
    pub index_count: u32,
}

/// A baked cluster-LOD mesh: shared vertex attributes + concatenated index pages
/// + per-cluster meta. `normals` / `uvs` / `colors` are empty when absent.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Clone, Debug, Default)]
pub struct ClusterMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub colors: Vec<[f32; 4]>,
    /// All clusters' triangles, concatenated; each cluster is a contiguous slice.
    pub indices: Vec<u32>,
    pub clusters: Vec<ClusterPage>,
}

impl ClusterMesh {
    /// Assemble the bake output from a built DAG plus the mesh's vertex
    /// attributes (the DAG clusters index these positions directly). Attribute
    /// vectors that are present must be parallel to `positions`.
    pub fn from_dag(
        dag: &ClusterDag,
        positions: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
        uvs: Vec<[f32; 2]>,
        colors: Vec<[f32; 4]>,
    ) -> Self {
        let total: usize = dag.clusters.iter().map(|c| c.triangles.len() * 3).sum();
        let mut indices = Vec::with_capacity(total);
        let mut clusters = Vec::with_capacity(dag.clusters.len());
        for c in &dag.clusters {
            let first_index = indices.len() as u32;
            for tri in &c.triangles {
                indices.extend_from_slice(tri);
            }
            clusters.push(ClusterPage {
                center: c.center,
                radius: c.radius,
                lod_error: c.lod_error,
                parent_error: c.parent_error,
                lod_bounds_center: c.lod_bounds_center,
                lod_bounds_radius: c.lod_bounds_radius,
                parent_bounds_center: c.parent_bounds_center,
                parent_bounds_radius: c.parent_bounds_radius,
                first_index,
                index_count: (c.triangles.len() * 3) as u32,
            });
        }
        ClusterMesh {
            positions,
            normals,
            uvs,
            colors,
            indices,
            clusters,
        }
    }

    pub fn cluster_count(&self) -> usize {
        self.clusters.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// The total triangles a renderer would draw if it took the **finest** cut
    /// (every level-0 cluster) — i.e. the source mesh's triangle count. Useful
    /// for a sanity check against the input.
    pub fn finest_triangle_count(&self) -> usize {
        self.clusters
            .iter()
            .filter(|c| c.lod_error == 0.0)
            .map(|c| (c.index_count / 3) as usize)
            .sum()
    }

    /// Cheap structural sanity check for a parsed DAG, before it's uploaded to the
    /// GPU. The bake's own output is always well-formed; this guards against a
    /// hand-authored, third-party, or corrupted `.clusters.bin` that would otherwise
    /// read out-of-bounds vertices or draw garbage. Returns `Err(reason)` on the
    /// first defect; the runtime logs it and refuses to materialize (renders nothing,
    /// rather than holes or a GPU OOB read).
    ///
    /// Checks only what would actually break the upload/draw, so a valid bake never
    /// trips it: every cluster page's index span lies within `indices`, each page is
    /// triangle-aligned, and every index references a real vertex.
    pub fn validate(&self) -> Result<(), String> {
        let n_idx = self.indices.len();
        let n_vert = self.positions.len();
        for (i, p) in self.clusters.iter().enumerate() {
            if p.index_count % 3 != 0 {
                return Err(format!(
                    "cluster {i}: index_count {} not a multiple of 3",
                    p.index_count
                ));
            }
            let end = p.first_index as usize + p.index_count as usize;
            if end > n_idx {
                return Err(format!(
                    "cluster {i}: index span [{}, {end}) exceeds index buffer len {n_idx}",
                    p.first_index
                ));
            }
        }
        if let Some(&bad) = self.indices.iter().find(|&&i| i as usize >= n_vert) {
            return Err(format!("index {bad} out of range for {n_vert} vertices"));
        }
        Ok(())
    }

    /// Assess this DAG's clustering quality against the source triangle count and
    /// return the degeneracy verdict ([`DagQuality`]). `source_triangles` is the
    /// pre-bake triangle count of the input mesh (use [`Self::finest_triangle_count`]
    /// if the source count isn't otherwise tracked — the finest cut reconstructs it).
    ///
    /// The SINGLE place this heuristic lives: the CLI and the editor bake both call
    /// it, so a degenerate `.clusters.bin` is dropped consistently in both paths.
    pub fn quality(&self, source_triangles: usize) -> DagQuality {
        let cluster_count = self.cluster_count();
        let dag_triangles = self.triangle_count();
        let avg_tris_per_cluster = dag_triangles as f32 / cluster_count.max(1) as f32;
        let dag_ratio = dag_triangles as f32 / source_triangles.max(1) as f32;
        let degenerate =
            avg_tris_per_cluster < MIN_AVG_TRIS_PER_CLUSTER || dag_ratio > MAX_DAG_TRI_RATIO;
        DagQuality {
            cluster_count,
            dag_triangles,
            avg_tris_per_cluster,
            dag_ratio,
            degenerate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::{build_cluster_dag, DagOptions};

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
    fn from_dag_pages_are_contiguous_and_in_range() {
        let (pos, indices) = grid(16);
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        let cm = ClusterMesh::from_dag(&dag, pos.clone(), vec![], vec![], vec![]);

        assert_eq!(cm.cluster_count(), dag.clusters.len());
        // Pages tile `indices` with no gaps or overlap, in order.
        let mut cursor = 0u32;
        for p in &cm.clusters {
            assert_eq!(p.first_index, cursor, "pages must be contiguous");
            assert!(p.index_count % 3 == 0);
            cursor += p.index_count;
            assert!(p.radius > 0.0);
            assert!(p.parent_error >= p.lod_error);
        }
        assert_eq!(cursor as usize, cm.indices.len());
        // Every index references a real vertex.
        assert!(cm.indices.iter().all(|&i| (i as usize) < pos.len()));
    }

    /// A normally-built DAG over a welded grid is healthy: dozens of tris/cluster,
    /// DAG total a small multiple of the source — `quality` must NOT flag it.
    #[test]
    fn quality_passes_healthy_dag() {
        let (pos, indices) = grid(32);
        let source_tris = indices.len() / 3;
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        let cm = ClusterMesh::from_dag(&dag, pos, vec![], vec![], vec![]);
        let q = cm.quality(source_tris);
        assert!(
            !q.degenerate,
            "healthy grid flagged degenerate: {:.1} tris/cluster, {:.1}× source",
            q.avg_tris_per_cluster, q.dag_ratio
        );
        assert!(q.avg_tris_per_cluster >= MIN_AVG_TRIS_PER_CLUSTER);
        assert!(q.dag_ratio <= MAX_DAG_TRI_RATIO);
    }

    /// A fully split-vertex mesh (no shared indices) with welding DISABLED can't
    /// cluster — adjacency collapses to ~1 tri/cluster. `quality` must flag it so
    /// the bake drops the DAG (the same case the CLI/editor guard catches). The
    /// healthy default path (welding on) is covered above + by the dag tests.
    #[test]
    fn quality_flags_degenerate_unwelded_split_mesh() {
        let (pos, indices) = grid(24);
        // Explode into per-triangle vertices so no two triangles share an index.
        let mut split_pos = Vec::with_capacity(indices.len());
        let mut split_idx = Vec::with_capacity(indices.len());
        for (i, &vi) in indices.iter().enumerate() {
            split_pos.push(pos[vi as usize]);
            split_idx.push(i as u32);
        }
        let source_tris = split_idx.len() / 3;
        let opts = DagOptions {
            weld_eps: None, // raw-index adjacency — the degenerate case
            ..DagOptions::default()
        };
        let dag = build_cluster_dag(&split_pos, &split_idx, &opts);
        let cm = ClusterMesh::from_dag(&dag, split_pos, vec![], vec![], vec![]);
        let q = cm.quality(source_tris);
        assert!(
            q.degenerate,
            "unwelded split mesh not flagged: {:.1} tris/cluster, {:.1}× source",
            q.avg_tris_per_cluster, q.dag_ratio
        );
    }

    /// A real bake validates; corrupting an index or a page span is caught.
    #[test]
    fn validate_accepts_real_bake_and_rejects_corruption() {
        let (pos, indices) = grid(16);
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        let cm = ClusterMesh::from_dag(&dag, pos, vec![], vec![], vec![]);
        assert!(cm.validate().is_ok(), "healthy bake must validate");

        // Out-of-range vertex index.
        let mut bad = cm.clone();
        *bad.indices.last_mut().unwrap() = bad.positions.len() as u32;
        assert!(bad.validate().is_err(), "OOB index must be rejected");

        // Page span past the end of the index buffer.
        let mut bad = cm.clone();
        bad.clusters[0].index_count = cm.indices.len() as u32 + 3;
        assert!(
            bad.validate().is_err(),
            "overlong page span must be rejected"
        );

        // Non-triangle-aligned page.
        let mut bad = cm.clone();
        bad.clusters[0].index_count += 1;
        assert!(
            bad.validate().is_err(),
            "non-multiple-of-3 page must be rejected"
        );
    }

    #[test]
    fn finest_cut_equals_source_triangles() {
        let (pos, indices) = grid(12);
        let total = indices.len() / 3;
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        let cm = ClusterMesh::from_dag(&dag, pos, vec![], vec![], vec![]);
        // The level-0 (error 0) clusters reconstruct exactly the source mesh.
        assert_eq!(cm.finest_triangle_count(), total);
    }

    /// The bundle serialises this with JSON (the codec shared by the editor bake
    /// and the scene-loader runtime). The root sentinel is finite, so it
    /// round-trips cleanly (infinity would break JSON).
    #[cfg(feature = "serde")]
    #[test]
    fn json_round_trip() {
        let (pos, indices) = grid(16);
        let dag = build_cluster_dag(&pos, &indices, &DagOptions::default());
        let cm = ClusterMesh::from_dag(&dag, pos, vec![], vec![], vec![]);
        let bytes = serde_json::to_vec(&cm).expect("serialize");
        let back: ClusterMesh = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(back.cluster_count(), cm.cluster_count());
        assert_eq!(back.indices, cm.indices);
        assert_eq!(back.clusters, cm.clusters);
        // No NaN/inf snuck into the page errors.
        assert!(back
            .clusters
            .iter()
            .all(|p| p.lod_error.is_finite() && p.parent_error.is_finite()));
    }
}
