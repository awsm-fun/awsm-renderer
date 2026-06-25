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
