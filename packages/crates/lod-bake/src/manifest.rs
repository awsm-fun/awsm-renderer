//! The per-mesh LOD descriptor written into the player bundle alongside the
//! level geometry, and read back by the runtime to drive level selection.
//!
//! Bundle layout for a LOD-baked mesh asset `<id>`:
//! ```text
//! assets/<id>.glb            ← level 0 (the base mesh, unchanged)
//! assets/<id>.lod1.glb       ← level 1 (simplified)
//! assets/<id>.lod2.glb       ← level 2 (coarser)
//! …
//! assets/<id>.lod.toml       ← this manifest
//! ```
//! The manifest is a *new* file; meshes without LOD have none, and the runtime
//! ignores it entirely when the `lod` feature is off — so a flag-off bundle is
//! byte-identical in everything the renderer reads.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Sidecar describing the simplified levels baked for one mesh asset.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshLodManifest {
    /// Object-space bounding-sphere radius of the base (level-0) mesh. Lets the
    /// runtime convert a level's object-space `error` into a projected
    /// screen-space error without re-reading the geometry.
    pub bounds_radius: f32,
    /// Base (level-0) triangle count, for reference / picking thresholds.
    pub base_triangle_count: u32,
    /// Simplified levels, finest-first (level 1, 2, …). Level 0 (the base
    /// `<id>.glb`) is implicit and not listed. Level `k` lives at
    /// `<id>.lod{k}.glb`.
    pub levels: Vec<MeshLodLevel>,
}

/// One simplified level of a mesh.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshLodLevel {
    /// 1-based file index — the level is stored at `<id>.lod{index}.glb`.
    pub index: u32,
    /// Object-space geometric error of this level, from the simplifier (the
    /// square root of the largest QEM cost paid). Monotonically non-decreasing
    /// across levels.
    pub error: f32,
    /// Triangle count at this level.
    pub triangle_count: u32,
}

/// The standard bundle filename for a mesh asset's LOD manifest.
pub fn lod_manifest_filename(asset_id: &str) -> String {
    format!("{asset_id}.lod.toml")
}

/// The standard bundle filename for a mesh asset's `level`-th simplified glb
/// (`level` is 1-based; level 0 is the base `<id>.glb`).
pub fn lod_level_filename(asset_id: &str, level: u32) -> String {
    format!("{asset_id}.lod{level}.glb")
}

/// Conservative object-space bounding-sphere radius: the max distance from the
/// AABB centre to any vertex. `0.0` for an empty mesh.
pub fn bounding_sphere_radius(positions: &[[f32; 3]]) -> f32 {
    if positions.is_empty() {
        return 0.0;
    }
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for p in positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let c = [
        0.5 * (lo[0] + hi[0]),
        0.5 * (lo[1] + hi[1]),
        0.5 * (lo[2] + hi[2]),
    ];
    let mut r2 = 0.0_f32;
    for p in positions {
        let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
        r2 = r2.max(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
    }
    r2.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_of_unit_cube() {
        // Corners of a 2×2×2 cube centred at origin → radius = sqrt(3).
        let cube = [
            [-1.0, -1.0, -1.0],
            [1.0, 1.0, 1.0],
            [1.0, -1.0, 1.0],
            [-1.0, 1.0, -1.0],
        ];
        let r = bounding_sphere_radius(&cube);
        assert!((r - 3.0_f32.sqrt()).abs() < 1e-5, "got {r}");
    }

    #[test]
    fn filenames() {
        assert_eq!(lod_manifest_filename("abc"), "abc.lod.toml");
        assert_eq!(lod_level_filename("abc", 2), "abc.lod2.glb");
    }
}
