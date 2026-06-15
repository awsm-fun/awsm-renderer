//! Mesh **introspection** — read-only geometry measurements that let an agent
//! perceive → self-correct (bbox / counts / volume / surface area / watertight,
//! and a silhouette profile along an axis). Pure functions over [`MeshData`],
//! natively unit-tested.

use std::collections::HashMap;

use glam::Vec3;

use crate::mesh_data::MeshData;

/// Aggregate geometry stats for a mesh.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MeshStats {
    pub vertices: usize,
    pub triangles: usize,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub centroid: [f32; 3],
    /// Sum of triangle areas.
    pub surface_area: f32,
    /// Signed volume via the divergence (tetrahedron) sum; meaningful for a
    /// closed, consistently-wound mesh (≈0 for open meshes).
    pub volume: f32,
    /// True when every edge is shared by exactly two triangles (closed manifold).
    pub watertight: bool,
}

/// Compute [`MeshStats`] for `mesh`.
pub fn mesh_stats(mesh: &MeshData) -> MeshStats {
    let mut bbox_min = [f32::INFINITY; 3];
    let mut bbox_max = [f32::NEG_INFINITY; 3];
    let mut sum = Vec3::ZERO;
    for p in &mesh.positions {
        for i in 0..3 {
            bbox_min[i] = bbox_min[i].min(p[i]);
            bbox_max[i] = bbox_max[i].max(p[i]);
        }
        sum += Vec3::from_array(*p);
    }
    if mesh.positions.is_empty() {
        bbox_min = [0.0; 3];
        bbox_max = [0.0; 3];
    }
    let centroid = if mesh.positions.is_empty() {
        Vec3::ZERO
    } else {
        sum / mesh.positions.len() as f32
    };

    let mut surface_area = 0.0;
    let mut volume6 = 0.0; // 6× signed volume
    for tri in mesh.indices.chunks_exact(3) {
        let a = Vec3::from_array(mesh.positions[tri[0] as usize]);
        let b = Vec3::from_array(mesh.positions[tri[1] as usize]);
        let c = Vec3::from_array(mesh.positions[tri[2] as usize]);
        surface_area += 0.5 * (b - a).cross(c - a).length();
        volume6 += a.dot(b.cross(c));
    }

    MeshStats {
        vertices: mesh.positions.len(),
        triangles: mesh.triangle_count(),
        bbox_min,
        bbox_max,
        centroid: centroid.to_array(),
        surface_area,
        volume: (volume6 / 6.0).abs(),
        watertight: is_watertight(mesh),
    }
}

/// Every undirected edge appears in exactly two triangles ⇒ closed manifold.
///
/// Vertices are first **welded by position** (quantized) so a mesh with split
/// per-face vertices (e.g. `box_mesh`, which duplicates corners per face for flat
/// shading) is judged on its real surface topology, not its index layout.
fn is_watertight(mesh: &MeshData) -> bool {
    if mesh.indices.is_empty() {
        return false;
    }
    // Quantize positions to weld coincident-but-split vertices.
    const Q: f32 = 1e4; // 1e-4 grid
    let key_of = |p: &[f32; 3]| {
        (
            (p[0] * Q).round() as i64,
            (p[1] * Q).round() as i64,
            (p[2] * Q).round() as i64,
        )
    };
    let mut canon: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let welded: Vec<u32> = mesh
        .positions
        .iter()
        .map(|p| {
            let next = canon.len() as u32;
            *canon.entry(key_of(p)).or_insert(next)
        })
        .collect();

    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for tri in mesh.indices.chunks_exact(3) {
        for k in 0..3 {
            let a = welded[tri[k] as usize];
            let b = welded[tri[(k + 1) % 3] as usize];
            if a == b {
                continue; // degenerate after welding
            }
            let key = if a < b { (a, b) } else { (b, a) };
            *edges.entry(key).or_insert(0) += 1;
        }
    }
    !edges.is_empty() && edges.values().all(|&c| c == 2)
}

/// The silhouette **profile** along `axis`: split the axis extent into `samples`
/// bins and, for each, report `[height, radius]` where `radius` is the max
/// distance of any vertex in the bin from the axis line (through the centroid).
/// Pairs with a [`MeshBase::Lathe`](crate::recipe::MeshBase) profile
/// — "measure the tip radius, adjust, re-measure".
pub fn cross_section_profile(mesh: &MeshData, axis: usize, samples: u32) -> Vec<[f32; 2]> {
    let n = samples.max(1) as usize;
    if mesh.positions.is_empty() || axis > 2 {
        return Vec::new();
    }
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    // Axis-line origin = centroid (its u/v components), so radius is measured
    // about the mesh's own center rather than the world origin.
    let stats = mesh_stats(mesh);
    let (cu, cv) = (stats.centroid[u], stats.centroid[v]);
    let lo = stats.bbox_min[axis];
    let hi = stats.bbox_max[axis];
    let span = (hi - lo).max(1e-6);

    let mut radius = vec![0.0_f32; n];
    for p in &mesh.positions {
        let t = ((p[axis] - lo) / span * n as f32).floor() as usize;
        let bin = t.min(n - 1);
        let du = p[u] - cu;
        let dv = p[v] - cv;
        let r = (du * du + dv * dv).sqrt();
        if r > radius[bin] {
            radius[bin] = r;
        }
    }
    (0..n)
        .map(|i| {
            // Bin center height.
            let h = lo + span * ((i as f32 + 0.5) / n as f32);
            [h, radius[i]]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modifiers::lathe;
    use crate::primitives::box_mesh;
    use std::f32::consts::TAU;

    #[test]
    fn cube_stats() {
        let m = box_mesh(Vec3::splat(2.0)); // 2×2×2 cube centered at origin
        let s = mesh_stats(&m);
        assert!(
            (s.surface_area - 24.0).abs() < 1e-3,
            "area {}",
            s.surface_area
        );
        assert!((s.volume - 8.0).abs() < 1e-3, "volume {}", s.volume);
        assert_eq!(s.bbox_min, [-1.0, -1.0, -1.0]);
        assert_eq!(s.bbox_max, [1.0, 1.0, 1.0]);
        for c in s.centroid {
            assert!(c.abs() < 1e-4);
        }
    }

    #[test]
    fn cube_is_watertight() {
        assert!(mesh_stats(&box_mesh(Vec3::ONE)).watertight);
    }

    #[test]
    fn open_strip_is_not_watertight() {
        // A single triangle: its edges are each used once.
        let m = MeshData {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: None,
            uvs: None,
            colors: None,
            indices: vec![0, 1, 2],
        };
        assert!(!mesh_stats(&m).watertight);
    }

    #[test]
    fn lathe_cylinder_cross_section_is_constant() {
        // A radius-1 cylinder built by lathing a 5-row constant-radius profile
        // (so every Y bin is populated), profiled along Y → max radius ≈ 1.
        let profile_rows: Vec<[f32; 2]> = (0..=4).map(|i| [i as f32 - 2.0, 1.0]).collect();
        let m = lathe(&profile_rows, 24, TAU);
        let profile = cross_section_profile(&m, 1, 5);
        assert_eq!(profile.len(), 5);
        for [_, r] in &profile {
            assert!((*r - 1.0).abs() < 0.02, "radius {r} not ≈ 1");
        }
    }

    #[test]
    fn sphere_cross_section_bulges_in_the_middle() {
        // A lathed semicircle profile → sphere-ish: mid bins wider than the ends.
        let rows: Vec<[f32; 2]> = (0..=8)
            .map(|i| {
                let t = i as f32 / 8.0; // 0..1
                let theta = t * std::f32::consts::PI; // 0..π
                [-theta.cos(), theta.sin().max(1e-3)] // height -cos, radius sin
            })
            .collect();
        let m = lathe(&rows, 24, TAU);
        let p = cross_section_profile(&m, 1, 9);
        let mid = p[p.len() / 2][1];
        let end = p[0][1].max(p[p.len() - 1][1]);
        assert!(mid > end + 0.3, "mid {mid} not wider than end {end}");
    }
}
