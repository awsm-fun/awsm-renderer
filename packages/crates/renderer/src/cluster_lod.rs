//! Runtime cluster-LOD (Phase B): the loaded cluster DAG + the LOD-cut
//! selection. The CPU [`select_cut`] here is the **reference spec** for the GPU
//! compute pass (B.2) — the same per-cluster rule runs on-device against the
//! uploaded cluster pages. Inert unless the `virtual_geometry` feature loads a
//! cluster mesh.
//!
//! **The cut.** Each cluster carries `[lod_error, parent_error)`. The DAG build
//! sets a child's `parent_error` equal to the `lod_error` of the coarser
//! clusters its group simplifies into, so these half-open intervals **tile**
//! `[0, ∞)` along every path through the DAG. Selecting
//! `{ c : lod_error <= t < parent_error }` therefore picks **exactly one**
//! cluster per surface region — a watertight cover — and the locked group
//! boundaries make the seam between adjacent detail levels crack-free.

use glam::{Mat4, Vec3};

/// One cluster's runtime page: bounds, LOD errors, and its index slice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClusterPage {
    /// Bounding-sphere centre (object space).
    pub center: [f32; 3],
    /// Bounding-sphere radius (object space).
    pub radius: f32,
    /// Error introduced creating this cluster (`0` at the finest level).
    pub lod_error: f32,
    /// Error of the group that simplifies this cluster away (root sentinel for
    /// roots).
    pub parent_error: f32,
    /// Group sphere (centre+radius) to project `lod_error` against. Group-shared,
    /// so all clusters of a group flip at the same camera threshold ⇒ crack-free.
    pub lod_bounds_center: [f32; 3],
    pub lod_bounds_radius: f32,
    /// Group sphere to project `parent_error` against.
    pub parent_bounds_center: [f32; 3],
    pub parent_bounds_radius: f32,
    /// First index of this cluster's triangles in the shared index buffer.
    pub first_index: u32,
    /// Index count (triangle count × 3).
    pub index_count: u32,
}

/// Select the LOD cut at a **uniform** object-space error `threshold`: every
/// cluster whose interval `[lod_error, parent_error)` contains it. Watertight by
/// the tiling argument above. Pushes cluster ids into `out` (cleared first;
/// reused across frames → no per-frame allocation).
pub fn select_cut(pages: &[ClusterPage], threshold: f32, out: &mut Vec<u32>) {
    out.clear();
    for (i, p) in pages.iter().enumerate() {
        if p.lod_error <= threshold && threshold < p.parent_error {
            out.push(i as u32);
        }
    }
}

/// **Per-cluster** LOD cut — the GPU cut's CPU reference. Selects each cluster
/// whose own projected error fits the `pixel_budget` but whose parent's doesn't,
/// projecting `lod_error` against its `lod_bounds` sphere and `parent_error`
/// against its `parent_bounds` sphere (both group-shared, so adjacent clusters of
/// a group flip together ⇒ crack-free). Because each cluster uses *its own*
/// distance, detail varies WITHIN one mesh: near clusters stay fine while far
/// clusters coarsen. Reuses `out` (no per-frame allocation). This is exactly what
/// the B.2 compute pass will evaluate per cluster on-device.
pub fn select_cut_per_cluster(
    pages: &[ClusterPage],
    instance_world: &Mat4,
    camera_pos: Vec3,
    tan_half_fov_y: f32,
    viewport_h: f32,
    pixel_budget: f32,
    out: &mut Vec<u32>,
) {
    out.clear();
    let scale = max_axis_scale(instance_world);
    for (i, p) in pages.iter().enumerate() {
        let lod_world = instance_world.transform_point3(Vec3::from(p.lod_bounds_center));
        let parent_world = instance_world.transform_point3(Vec3::from(p.parent_bounds_center));
        let proj_lod = cluster_projected_error(
            p.lod_error,
            lod_world,
            camera_pos,
            tan_half_fov_y,
            viewport_h,
            scale,
        );
        let proj_parent = cluster_projected_error(
            p.parent_error,
            parent_world,
            camera_pos,
            tan_half_fov_y,
            viewport_h,
            scale,
        );
        if proj_lod <= pixel_budget && pixel_budget < proj_parent {
            out.push(i as u32);
        }
    }
}

/// Object-space error budget for a whole instance at uniform detail: the
/// pixel budget back-projected to object space at the instance's distance.
/// `select_cut(pages, instance_error_threshold(...))` then yields a watertight,
/// per-instance LOD that coarsens with distance (the simple cut; the GPU pass
/// refines to per-cluster distances using group-consistent bounds — see B.2).
pub fn instance_error_threshold(
    instance_world: &Mat4,
    camera_pos: Vec3,
    tan_half_fov_y: f32,
    viewport_h: f32,
    pixel_budget: f32,
) -> f32 {
    let center = instance_world.transform_point3(Vec3::ZERO);
    let dist = (center - camera_pos).length();
    let scale = max_axis_scale(instance_world);
    // pixels = error * scale * (viewport_h/2) / (dist * tan) ⇒
    // error_for_budget = budget * dist * tan / (scale * viewport_h/2)
    let denom = scale * (viewport_h * 0.5);
    if denom <= 1e-9 || tan_half_fov_y <= 1e-9 {
        return 0.0; // degenerate ⇒ finest
    }
    pixel_budget * dist * tan_half_fov_y / denom
}

/// Project an object-space error to screen pixels for a cluster centred at
/// `world_center` (the per-cluster form the GPU pass uses; shown here as the
/// reference). `+∞` for a degenerate distance/FOV.
pub fn cluster_projected_error(
    error: f32,
    world_center: Vec3,
    camera_pos: Vec3,
    tan_half_fov_y: f32,
    viewport_h: f32,
    world_scale: f32,
) -> f32 {
    let dist = (world_center - camera_pos).length();
    if dist <= 1e-6 || tan_half_fov_y <= 1e-6 {
        return f32::INFINITY;
    }
    error * world_scale * (viewport_h * 0.5) / (dist * tan_half_fov_y)
}

/// Largest world-space axis scale of an object→world transform.
pub fn max_axis_scale(m: &Mat4) -> f32 {
    m.x_axis
        .truncate()
        .length()
        .max(m.y_axis.truncate().length())
        .max(m.z_axis.truncate().length())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3-level synthetic DAG: 4 finest clusters → 2 mid → 1 root, with the
    /// child `parent_error` matching the parent `lod_error` (intervals tile).
    fn synthetic() -> Vec<ClusterPage> {
        let mk = |lod, parent, tris: u32| ClusterPage {
            center: [0.0, 0.0, 0.0],
            radius: 1.0,
            lod_error: lod,
            parent_error: parent,
            lod_bounds_center: [0.0, 0.0, 0.0],
            lod_bounds_radius: 1.0,
            parent_bounds_center: [0.0, 0.0, 0.0],
            parent_bounds_radius: 1.0,
            first_index: 0,
            index_count: tris * 3,
        };
        vec![
            mk(0.0, 1.0, 10), // level 0 ×4
            mk(0.0, 1.0, 10),
            mk(0.0, 1.0, 10),
            mk(0.0, 1.0, 10),
            mk(1.0, 2.0, 12), // level 1 ×2
            mk(1.0, 2.0, 12),
            mk(2.0, f32::INFINITY, 8), // root
        ]
    }

    fn cut_tris(pages: &[ClusterPage], t: f32) -> u32 {
        let mut out = Vec::new();
        select_cut(pages, t, &mut out);
        out.iter().map(|&i| pages[i as usize].index_count / 3).sum()
    }

    #[test]
    fn finest_cut_at_zero() {
        let p = synthetic();
        let mut out = Vec::new();
        select_cut(&p, 0.0, &mut out);
        assert_eq!(out, vec![0, 1, 2, 3], "threshold 0 picks the finest level");
        assert_eq!(cut_tris(&p, 0.0), 40);
    }

    #[test]
    fn mid_and_root_cuts() {
        let p = synthetic();
        let mut out = Vec::new();
        select_cut(&p, 1.5, &mut out);
        assert_eq!(out, vec![4, 5], "1<=1.5<2 picks the mid level");
        select_cut(&p, 5.0, &mut out);
        assert_eq!(out, vec![6], "above all finite errors picks the root");
    }

    #[test]
    fn triangle_count_is_monotone_non_increasing() {
        let p = synthetic();
        let mut prev = u32::MAX;
        for t in [0.0f32, 0.5, 1.0, 1.5, 2.0, 3.0, 100.0] {
            let n = cut_tris(&p, t);
            assert!(n > 0, "the cut always covers the surface");
            assert!(n <= prev, "coarser threshold must not increase triangles");
            prev = n;
        }
    }

    #[test]
    fn every_cluster_is_selected_at_its_lower_bound() {
        // Each cluster appears in the cut at exactly its own lod_error.
        let p = synthetic();
        for (i, page) in p.iter().enumerate() {
            let mut out = Vec::new();
            select_cut(&p, page.lod_error, &mut out);
            assert!(out.contains(&(i as u32)), "cluster {i} missing at its lod_error");
        }
    }

    #[test]
    fn instance_threshold_coarsens_with_distance() {
        let world = Mat4::IDENTITY;
        let near = instance_error_threshold(&world, Vec3::new(0.0, 0.0, 2.0), 0.5, 1080.0, 1.0);
        let far = instance_error_threshold(&world, Vec3::new(0.0, 0.0, 50.0), 0.5, 1080.0, 1.0);
        assert!(far > near, "a farther instance tolerates a larger object error");
        // Reuse-buffer call doesn't allocate a fresh vec each time.
        let p = synthetic();
        let mut out = Vec::new();
        select_cut(&p, near, &mut out);
        let near_tris: u32 = out.iter().map(|&i| p[i as usize].index_count / 3).sum();
        select_cut(&p, far, &mut out);
        let far_tris: u32 = out.iter().map(|&i| p[i as usize].index_count / 3).sum();
        assert!(far_tris <= near_tris);
    }

    #[test]
    fn per_cluster_cut_varies_detail_by_distance() {
        // The headline property of the GPU cut: detail varies WITHIN a mesh.
        // Two regions — A at the origin (near the camera), B far down +X — each
        // with a fine cluster (lod 0, small parent error) and a coarse cluster
        // (lod 0.1, root). With one budget, region A should keep its FINE cluster
        // while region B drops to its COARSE one.
        let page = |cx: f32, lod: f32, parent: f32, tris: u32| ClusterPage {
            center: [cx, 0.0, 0.0],
            radius: 1.0,
            lod_error: lod,
            parent_error: parent,
            lod_bounds_center: [cx, 0.0, 0.0],
            lod_bounds_radius: 1.0,
            parent_bounds_center: [cx, 0.0, 0.0],
            parent_bounds_radius: 1.0,
            first_index: 0,
            index_count: tris * 3,
        };
        let pages = vec![
            page(0.0, 0.0, 0.1, 100),            // 0: A fine
            page(0.0, 0.1, f32::INFINITY, 30),   // 1: A coarse
            page(100.0, 0.0, 0.1, 100),          // 2: B fine
            page(100.0, 0.1, f32::INFINITY, 30), // 3: B coarse
        ];
        let mut out = Vec::new();
        select_cut_per_cluster(
            &pages,
            &Mat4::IDENTITY,
            Vec3::new(0.0, 0.0, 3.0), // near A, far from B
            0.5,
            1080.0,
            2.0, // pixel budget
            &mut out,
        );
        out.sort_unstable();
        assert!(out.contains(&0), "near region keeps its FINE cluster");
        assert!(out.contains(&3), "far region drops to its COARSE cluster");
        assert!(!out.contains(&1), "near region must not pick its coarse cluster");
        assert!(!out.contains(&2), "far region must not pick its fine cluster");
    }
}
