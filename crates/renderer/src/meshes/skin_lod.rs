//! Distance-based skinning-LOD helpers (Cluster 8.3).
//!
//! Skinned characters far from the camera don't need a per-frame joint
//! matrix refresh — the visible motion is well below per-pixel
//! resolution past a few metres. `set_skin_update_periods_by_distance`
//! walks the spatial index and assigns each skinned mesh's
//! `skin_update_period` from a user-supplied threshold table.
//!
//! Pair with Cluster 6.2's coverage-driven skinning skip: coverage
//! answers "skip this frame entirely?", period answers "what's the
//! background cadence when not skipped?".

use glam::Vec3;

use crate::{meshes::MeshKey, AwsmRenderer};

/// One row of the distance → period table. Meshes whose AABB-center
/// distance to the camera is below `max_distance` and above the
/// previous row's `max_distance` receive `period` as their skinning
/// cadence.
#[derive(Clone, Copy, Debug)]
pub struct SkinLodLevel {
    pub max_distance: f32,
    pub period: u8,
}

impl AwsmRenderer {
    /// Sets a single mesh's `skin_update_period`. `1` updates every
    /// frame (default); `2` halves the cost; `4` quarter-rate.
    pub fn set_mesh_skin_update_period(
        &mut self,
        mesh_key: MeshKey,
        period: u8,
    ) -> crate::error::Result<()> {
        let mesh = self.meshes.get_mut(mesh_key)?;
        mesh.skin_update_period = period.max(1);
        Ok(())
    }

    /// Auto-assigns `skin_update_period` for every skinned mesh based
    /// on its AABB-center distance to `camera_pos`. The `levels` table
    /// is expected to be sorted by ascending `max_distance`; the first
    /// matching row wins. Meshes beyond the last `max_distance` get
    /// the last row's period.
    ///
    /// Cheap — one BVH `iter_all` plus an O(meshes) distance compute.
    /// Call this on a slow tick (every ~10 frames, or when the camera
    /// crosses a coarse grid) rather than every frame.
    pub fn set_skin_update_periods_by_distance(
        &mut self,
        camera_pos: Vec3,
        levels: &[SkinLodLevel],
    ) {
        if levels.is_empty() {
            return;
        }
        let snapshot: Vec<(MeshKey, Vec3)> = self
            .scene_spatial
            .iter_all()
            .map(|node| (node.mesh_key, node.aabb.center()))
            .collect();
        for (mesh_key, center) in snapshot {
            // Skip non-skinned meshes — no skin to throttle.
            let has_skin = self
                .meshes
                .mesh_skin_key(mesh_key)
                .map(|opt| opt.is_some())
                .unwrap_or(false);
            if !has_skin {
                continue;
            }
            let dist = (center - camera_pos).length();
            let period = levels
                .iter()
                .find(|lvl| dist <= lvl.max_distance)
                .map(|lvl| lvl.period)
                .unwrap_or_else(|| levels.last().map(|l| l.period).unwrap_or(1))
                .max(1);
            if let Ok(mesh) = self.meshes.get_mut(mesh_key) {
                mesh.skin_update_period = period;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_levels_pick_first_match() {
        let levels = [
            SkinLodLevel {
                max_distance: 10.0,
                period: 1,
            },
            SkinLodLevel {
                max_distance: 30.0,
                period: 2,
            },
            SkinLodLevel {
                max_distance: 80.0,
                period: 4,
            },
        ];
        // Simulate the find() the function uses.
        let pick = |d: f32| -> u8 {
            levels
                .iter()
                .find(|lvl| d <= lvl.max_distance)
                .map(|lvl| lvl.period)
                .unwrap_or_else(|| levels.last().unwrap().period)
        };
        assert_eq!(pick(5.0), 1);
        assert_eq!(pick(20.0), 2);
        assert_eq!(pick(50.0), 4);
        assert_eq!(
            pick(200.0),
            4,
            "past last threshold, sticks at slowest tier"
        );
    }
}
