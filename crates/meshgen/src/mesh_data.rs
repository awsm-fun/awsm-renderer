//! `MeshData` plain-data struct + `compute_vertex_normals` helper.

use glam::Vec3;

/// Plain-data mesh representation. The renderer's raw-mesh API consumes this directly.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Option<Vec<[f32; 3]>>,
    pub uvs: Option<Vec<[f32; 2]>>,
    pub colors: Option<Vec<[f32; 4]>>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Compute per-vertex normals as the area-weighted average of incident face normals.
    /// Overwrites any existing normals.
    pub fn compute_vertex_normals(&mut self) {
        let mut acc = vec![Vec3::ZERO; self.positions.len()];
        let positions: Vec<Vec3> = self.positions.iter().map(|p| Vec3::from_array(*p)).collect();
        for tri in self.indices.chunks_exact(3) {
            let i0 = tri[0] as usize;
            let i1 = tri[1] as usize;
            let i2 = tri[2] as usize;
            let a = positions[i0];
            let b = positions[i1];
            let c = positions[i2];
            let n = (b - a).cross(c - a);
            acc[i0] += n;
            acc[i1] += n;
            acc[i2] += n;
        }
        self.normals = Some(
            acc.into_iter()
                .map(|n| n.normalize_or_zero().to_array())
                .collect(),
        );
    }
}

/// Free-function form of `MeshData::compute_vertex_normals`.
pub fn compute_vertex_normals(mesh: &mut MeshData) {
    mesh.compute_vertex_normals();
}
