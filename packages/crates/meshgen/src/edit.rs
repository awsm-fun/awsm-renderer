//! Raw per-vertex editing primitives (Phase 4 core): soft (falloff) transforms
//! and predicate-based vertex selection. Pure functions over [`MeshData`] —
//! natively unit-tested — so the editor's `SoftTransformVertices` /
//! `select_vertices_where` commands are thin wrappers (resolve mesh bytes → call
//! these → store the sparse diff).

use glam::Vec3;

use crate::mesh_data::MeshData;

/// Comparison for [`select_by_axis`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Greater,
    Less,
}

/// Translate the `selected` vertices by `translation`, dragging nearby vertices
/// with a smooth falloff: a vertex within `falloff` of the nearest selected
/// vertex moves by `translation * w`, where `w` smoothsteps 1→0 over the radius
/// (selected vertices move fully). `falloff <= 0` ⇒ a hard move of exactly the
/// selection. Returns the **full** new position array (the caller diffs it).
pub fn soft_transform_positions(
    mesh: &MeshData,
    selected: &[u32],
    translation: [f32; 3],
    falloff: f32,
) -> Vec<[f32; 3]> {
    let t = Vec3::from_array(translation);
    let sel_positions: Vec<Vec3> = selected
        .iter()
        .filter_map(|&i| mesh.positions.get(i as usize))
        .map(|p| Vec3::from_array(*p))
        .collect();
    let selected_set: std::collections::HashSet<u32> = selected.iter().copied().collect();

    mesh.positions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let pos = Vec3::from_array(*p);
            let w = if selected_set.contains(&(i as u32)) {
                1.0
            } else if falloff > 0.0 && !sel_positions.is_empty() {
                let d = sel_positions
                    .iter()
                    .map(|s| (pos - *s).length())
                    .fold(f32::INFINITY, f32::min);
                if d < falloff {
                    smoothstep(1.0 - d / falloff)
                } else {
                    0.0
                }
            } else {
                0.0
            };
            (pos + t * w).to_array()
        })
        .collect()
}

/// Smoothstep on `[0,1]` (3t² − 2t³).
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Select vertices whose normal points within `threshold` (dot > threshold) of
/// `dir` — e.g. "the top-facing verts" (`dir = +Y`). Needs normals.
pub fn select_by_normal_dir(mesh: &MeshData, dir: [f32; 3], threshold: f32) -> Vec<u32> {
    let Some(normals) = &mesh.normals else {
        return Vec::new();
    };
    let d = Vec3::from_array(dir).normalize_or_zero();
    normals
        .iter()
        .enumerate()
        .filter(|(_, n)| Vec3::from_array(**n).normalize_or_zero().dot(d) > threshold)
        .map(|(i, _)| i as u32)
        .collect()
}

/// Select vertices on one side of an axis plane: component `axis` `cmp` `value`.
pub fn select_by_axis(mesh: &MeshData, axis: usize, cmp: Cmp, value: f32) -> Vec<u32> {
    if axis > 2 {
        return Vec::new();
    }
    mesh.positions
        .iter()
        .enumerate()
        .filter(|(_, p)| match cmp {
            Cmp::Greater => p[axis] > value,
            Cmp::Less => p[axis] < value,
        })
        .map(|(i, _)| i as u32)
        .collect()
}

/// Select every vertex within the top `percent` (0..1) of the axis **extent**
/// along `axis` — a height band (the count depends on tessellation, not `percent`).
pub fn select_top_percent_axis(mesh: &MeshData, axis: usize, percent: f32) -> Vec<u32> {
    if axis > 2 || mesh.positions.is_empty() {
        return Vec::new();
    }
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for p in &mesh.positions {
        lo = lo.min(p[axis]);
        hi = hi.max(p[axis]);
    }
    let cutoff = hi - (hi - lo) * percent.clamp(0.0, 1.0);
    select_by_axis(mesh, axis, Cmp::Greater, cutoff)
}

/// Select the `count` vertices with the GREATEST value along `axis` (a count, not
/// a height band; ties broken by ascending index). Returned in ascending index
/// order, like the other selectors. The count-based companion to
/// [`select_top_percent_axis`].
pub fn select_top_count_axis(mesh: &MeshData, axis: usize, count: u32) -> Vec<u32> {
    if axis > 2 || mesh.positions.is_empty() || count == 0 {
        return Vec::new();
    }
    let mut idx: Vec<u32> = (0..mesh.positions.len() as u32).collect();
    // Highest axis value first; stable by index so ties are deterministic.
    idx.sort_by(|&a, &b| {
        mesh.positions[b as usize][axis]
            .partial_cmp(&mesh.positions[a as usize][axis])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    idx.truncate(count as usize);
    idx.sort_unstable();
    idx
}

/// Select vertices within `radius` of `center`.
pub fn select_within_radius(mesh: &MeshData, center: [f32; 3], radius: f32) -> Vec<u32> {
    let c = Vec3::from_array(center);
    mesh.positions
        .iter()
        .enumerate()
        .filter(|(_, p)| (Vec3::from_array(**p) - c).length() <= radius)
        .map(|(i, _)| i as u32)
        .collect()
}

/// Select vertices inside the axis-aligned box `[min, max]` (inclusive). The box
/// is in the mesh's local space (pair with `get_node_bounds`, transforming a
/// world box into local first, for region selection by area).
pub fn select_within_aabb(mesh: &MeshData, min: [f32; 3], max: [f32; 3]) -> Vec<u32> {
    mesh.positions
        .iter()
        .enumerate()
        .filter(|(_, p)| (0..3).all(|a| p[a] >= min[a] && p[a] <= max[a]))
        .map(|(i, _)| i as u32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modifiers::lathe;
    use crate::primitives::box_mesh;
    use std::f32::consts::TAU;

    #[test]
    fn within_aabb_selects_the_box_region() {
        // A 2x2x2 box centered at origin → 8 corners at ±1. A box covering only
        // the +x half (x in [0,2]) selects exactly the 4 +x corners.
        let m = box_mesh(Vec3::splat(2.0));
        let sel = select_within_aabb(&m, [0.0, -2.0, -2.0], [2.0, 2.0, 2.0]);
        assert!(!sel.is_empty());
        assert!(sel.iter().all(|&i| m.positions[i as usize][0] >= 0.0));
        // Every +x vertex is selected; no -x vertex is.
        for (i, p) in m.positions.iter().enumerate() {
            assert_eq!(sel.contains(&(i as u32)), p[0] >= 0.0);
        }
    }

    #[test]
    fn hard_transform_moves_only_selection() {
        let m = box_mesh(Vec3::splat(2.0));
        let out = soft_transform_positions(&m, &[0], [10.0, 0.0, 0.0], 0.0);
        // Vertex 0 moved by exactly +10x; everything else stayed put.
        assert!((out[0][0] - (m.positions[0][0] + 10.0)).abs() < 1e-5);
        for (o, p) in out.iter().zip(&m.positions).skip(1) {
            assert_eq!(o, p);
        }
    }

    #[test]
    fn soft_transform_falls_off_with_distance() {
        // A row of vertices along X; drag vertex 0 with a falloff covering ~half.
        let m = MeshData {
            positions: (0..5).map(|i| [i as f32, 0.0, 0.0]).collect(),
            normals: None,
            uvs: None,
            colors: None,
            indices: vec![],
        };
        let out = soft_transform_positions(&m, &[0], [0.0, 1.0, 0.0], 2.5);
        // Selected fully moved; weight strictly decreases with distance; beyond
        // the radius unaffected.
        assert!((out[0][1] - 1.0).abs() < 1e-5);
        assert!(out[1][1] > out[2][1] && out[2][1] > 0.0);
        assert_eq!(out[3][1], 0.0); // distance 3 > radius 2.5
        assert_eq!(out[4][1], 0.0);
    }

    #[test]
    fn select_top_percent_grabs_the_cap() {
        // A lathed cylinder, y in [-2, 2]; top 25% ⇒ only y > 1.
        let rows: Vec<[f32; 2]> = (0..=4).map(|i| [i as f32 - 2.0, 1.0]).collect();
        let m = lathe(&rows, 12, TAU);
        let sel = select_top_percent_axis(&m, 1, 0.25);
        assert!(!sel.is_empty());
        for &i in &sel {
            assert!(m.positions[i as usize][1] > 1.0);
        }
    }

    #[test]
    fn select_top_count_grabs_exactly_n_highest() {
        // 5 verts stacked along Y at y = 0..4. Top 2 by count = the two highest.
        let m = MeshData {
            positions: (0..5).map(|i| [0.0, i as f32, 0.0]).collect(),
            normals: None,
            uvs: None,
            colors: None,
            indices: vec![],
        };
        let sel = select_top_count_axis(&m, 1, 2);
        assert_eq!(
            sel,
            vec![3, 4],
            "exactly the 2 highest verts, in index order"
        );
        // count 0 selects nothing; count beyond the vertex count clamps to all.
        assert!(select_top_count_axis(&m, 1, 0).is_empty());
        assert_eq!(select_top_count_axis(&m, 1, 99).len(), 5);
    }

    #[test]
    fn select_within_radius_and_axis() {
        let m = box_mesh(Vec3::splat(2.0)); // corners at ±1
        let near = select_within_radius(&m, [1.0, 1.0, 1.0], 0.1);
        assert!(near.iter().all(|&i| {
            let p = m.positions[i as usize];
            (p[0] - 1.0).abs() < 0.1 && (p[1] - 1.0).abs() < 0.1 && (p[2] - 1.0).abs() < 0.1
        }));
        let pos_x = select_by_axis(&m, 0, Cmp::Greater, 0.0);
        assert!(pos_x.iter().all(|&i| m.positions[i as usize][0] > 0.0));
    }

    #[test]
    fn select_by_normal_dir_top_faces() {
        let m = box_mesh(Vec3::splat(2.0)); // box_mesh sets per-face normals
        let up = select_by_normal_dir(&m, [0.0, 1.0, 0.0], 0.9);
        assert!(!up.is_empty());
        for &i in &up {
            assert!(m.normals.as_ref().unwrap()[i as usize][1] > 0.9);
        }
    }
}
