//! Discrete LOD-chain planning: decide *which* levels to emit for a mesh and
//! describe them, without touching any file format. The caller (the editor
//! bake) turns each [`PlannedLevel`] into a glb by gathering its surviving-vertex
//! attributes, and writes [`LodPlan::manifest`] as the sidecar.
//!
//! Keeping the policy here (the min-triangle floor, dropping non-reducing
//! levels, level numbering, error/tri bookkeeping) makes it unit-testable in a
//! plain native crate — the editor side is then only attribute gather + glb
//! encode + filename.

use crate::manifest::{bounding_sphere_radius, MeshLodLevel, MeshLodManifest};
use crate::simplify::{build_lod_chain, SimplifiedMesh};

/// One level the bake should emit.
#[derive(Clone, Debug)]
pub struct PlannedLevel {
    /// 1-based file index — the level is stored at `<id>.lod{index}.glb`.
    pub index: u32,
    /// The simplified geometry: a surviving-vertex remap + remapped indices.
    /// Gather any per-vertex attribute through it with [`SimplifiedMesh::gather`].
    pub mesh: SimplifiedMesh,
}

/// The full plan for a mesh: bounds + base size + the levels to emit.
#[derive(Clone, Debug)]
pub struct LodPlan {
    pub bounds_radius: f32,
    pub base_triangle_count: u32,
    /// Emitted levels in finest-first order. Empty ⇒ no LOD for this mesh.
    pub levels: Vec<PlannedLevel>,
}

impl LodPlan {
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// The bundle manifest describing this plan.
    pub fn manifest(&self) -> MeshLodManifest {
        MeshLodManifest {
            bounds_radius: self.bounds_radius,
            base_triangle_count: self.base_triangle_count,
            levels: self
                .levels
                .iter()
                .map(|l| MeshLodLevel {
                    index: l.index,
                    error: l.mesh.error,
                    triangle_count: l.mesh.triangle_count() as u32,
                })
                .collect(),
        }
    }
}

/// Plan the discrete LOD chain for `(positions, indices)`:
/// - returns an empty plan (no levels) when the base is below `min_triangles`;
/// - runs the simplifier once per `ratio`;
/// - drops any level that didn't reduce the triangle count versus the previous
///   emitted level (e.g. boundary-locked geometry that can't shrink further),
///   so the chain is strictly decreasing and never ships a duplicate;
/// - numbers the survivors `1, 2, …`.
pub fn plan_lod_levels(
    positions: &[[f32; 3]],
    indices: &[u32],
    ratios: &[f32],
    min_triangles: usize,
) -> LodPlan {
    let base_tris = indices.len() / 3;
    let mut plan = LodPlan {
        bounds_radius: bounding_sphere_radius(positions),
        base_triangle_count: base_tris as u32,
        levels: Vec::new(),
    };
    if base_tris < min_triangles {
        return plan;
    }

    let mut prev = base_tris;
    let mut index = 1u32;
    for mesh in build_lod_chain(positions, indices, ratios) {
        let tris = mesh.triangle_count();
        if tris == 0 || tris >= prev {
            continue;
        }
        prev = tris;
        plan.levels.push(PlannedLevel { index, mesh });
        index += 1;
    }
    plan
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
    fn below_floor_is_empty() {
        let (pos, idx) = grid(4); // 32 tris
        let plan = plan_lod_levels(&pos, &idx, &[0.5, 0.25], 512);
        assert!(plan.is_empty());
        assert_eq!(plan.base_triangle_count, 32);
        assert!(plan.manifest().levels.is_empty());
    }

    #[test]
    fn levels_strictly_decrease_and_number_from_one() {
        let (pos, idx) = grid(24); // 1152 tris
        let plan = plan_lod_levels(&pos, &idx, &[0.5, 0.25, 0.125], 512);
        assert!(!plan.is_empty());
        // Indices are 1,2,3,… contiguous.
        for (i, lvl) in plan.levels.iter().enumerate() {
            assert_eq!(lvl.index, i as u32 + 1);
        }
        // Triangle counts strictly decrease from the base; errors non-decreasing.
        let m = plan.manifest();
        assert_eq!(m.base_triangle_count, 1152);
        let mut prev_tris = m.base_triangle_count + 1;
        let mut prev_err = -1.0f32;
        for lvl in &m.levels {
            assert!(lvl.triangle_count < prev_tris, "tris not decreasing");
            assert!(lvl.error >= prev_err, "error not monotone");
            prev_tris = lvl.triangle_count;
            prev_err = lvl.error;
        }
    }

    #[test]
    fn manifest_carries_bounds() {
        let (pos, idx) = grid(24);
        let plan = plan_lod_levels(&pos, &idx, &[0.5], 512);
        assert!(plan.manifest().bounds_radius > 0.0);
    }
}
