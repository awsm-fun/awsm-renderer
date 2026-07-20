//! Discrete level-of-detail: per-mesh simplified level chains + screen-error
//! level selection.
//!
//! The bake (`awsm-renderer-lod-bake`, consumed in the editor/player bundle)
//! emits, per LOD-enabled mesh, a chain of progressively simplified levels —
//! each a distinct geometry. At load time (when the [`lod`] feature is on) the
//! scene loader registers every level as its own [`MeshKey`] and records the
//! chain here, keyed by the **base** mesh's key. Each frame the renderer picks a
//! level per instance with [`select_level`] and reroutes that instance's draw to
//! the chosen level's key — so "each level = a `MeshKey`" and the existing
//! cull / compaction / geometry pipeline draws levels as ordinary meshes.
//!
//! With the feature off the registry is empty, every instance draws its base
//! mesh, and there is no behavioural difference from a build without LOD.
//!
//! [`lod`]: crate::features::RendererFeatures::lod

use slotmap::SecondaryMap;

use crate::meshes::MeshKey;

/// One simplified level of a base mesh.
#[derive(Clone, Copy, Debug)]
pub struct LodLevel {
    /// The level's registered geometry — a distinct [`MeshKey`] from the base.
    pub mesh_key: MeshKey,
    /// Object-space geometric error from the bake (sqrt of the max QEM cost).
    /// Monotonically non-decreasing along the chain.
    pub error: f32,
}

/// The simplified levels for one base mesh, finest-first (level 1, 2, …). Level
/// 0 is the base mesh itself (the chain's owning key), not stored here.
#[derive(Clone, Debug, Default)]
pub struct LodChain {
    pub levels: Vec<LodLevel>,
    /// Object-space bounding-sphere radius of the base mesh — reserved for
    /// bounds-aware selection refinements; the error is already absolute so the
    /// basic projection in [`select_level`] doesn't need it.
    pub bounds_radius: f32,
    /// The level currently shown for this chain (`0` = base). Tracked so the
    /// per-frame selection only calls `set_mesh_hidden` when the choice actually
    /// changes — steady-state cost is the selection arithmetic, no flag churn.
    /// Starts at `0`, matching the loader (levels loaded hidden, base visible).
    pub current_level: usize,
}

impl LodChain {
    /// Resolve a selected level index to the key to draw: `0` → `base`, else the
    /// `level`-th simplified key. Panics only on an out-of-range index, which
    /// [`select_level`] never returns for this chain.
    pub fn key_for_level(&self, base: MeshKey, level: usize) -> MeshKey {
        if level == 0 {
            base
        } else {
            self.levels[level - 1].mesh_key
        }
    }
}

/// Per-base-`MeshKey` LOD chains. Empty unless the `lod` feature loaded a bundle
/// that carried level geometry.
#[derive(Default)]
pub struct LodRegistry {
    chains: SecondaryMap<MeshKey, LodChain>,
}

impl LodRegistry {
    pub fn is_empty(&self) -> bool {
        self.chains.is_empty()
    }

    /// Record the level chain for a base mesh. Replaces any existing entry.
    pub fn register(&mut self, base: MeshKey, chain: LodChain) {
        self.chains.insert(base, chain);
    }

    /// Remove a chain (authored far-swap teardown). Returns it so the caller
    /// can restore visibility (un-hide the base, re-hide/show levels as its
    /// ownership dictates).
    pub fn unregister(&mut self, base: MeshKey) -> Option<LodChain> {
        self.chains.remove(base)
    }

    pub fn get(&self, base: MeshKey) -> Option<&LodChain> {
        self.chains.get(base)
    }

    /// Mutable iteration over `(base MeshKey, chain)` — the per-frame selection
    /// pass uses this to update each chain's `current_level`.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (MeshKey, &mut LodChain)> {
        self.chains.iter_mut()
    }

    pub fn clear(&mut self) {
        self.chains.clear();
    }

    /// Every simplified-level key across all chains. These are registered as
    /// ordinary meshes but must be kept out of the normal renderable/draw list —
    /// they only draw when an instance's selection reroutes to them.
    pub fn level_keys(&self) -> impl Iterator<Item = MeshKey> + '_ {
        self.chains
            .values()
            .flat_map(|c| c.levels.iter().map(|l| l.mesh_key))
    }
}

/// Pixels subtended by one object-space unit at `distance`, for a perspective
/// camera with vertical half-FOV tangent `tan_half_fov_y` rendering into a
/// viewport `viewport_h` pixels tall. A length `L` projects to
/// `L · (viewport_h/2) / (distance · tan(fov_y/2))` pixels. Returns `+∞` for a
/// degenerate distance/FOV (so everything reads as "very close" → base level).
pub fn projected_px_per_unit(distance: f32, tan_half_fov_y: f32, viewport_h: f32) -> f32 {
    if distance <= 1e-6 || tan_half_fov_y <= 1e-6 {
        return f32::INFINITY;
    }
    (viewport_h * 0.5) / (distance * tan_half_fov_y)
}

/// Select the LOD level for an instance: `0` = base, `k` = `chain.levels[k-1]`.
///
/// Picks the **coarsest** level whose projected screen-space error stays within
/// `error_threshold_px`. A level's projected error is
/// `level.error · error_conservatism · world_scale · px_per_unit`. Errors are
/// monotonically non-decreasing along the chain, so the first level that
/// exceeds the budget caps the choice (everything coarser is worse).
///
/// `world_scale` is the instance's largest world-space axis scale (errors are in
/// the mesh's object space; a scaled-up instance projects its error larger).
///
/// `error_conservatism` calibrates the bake's raw sqrt-QEM (≈RMS deviation)
/// metric to perceptual error — see
/// [`AwsmRenderer::LOD_ERROR_CONSERVATISM`](crate::AwsmRenderer::LOD_ERROR_CONSERVATISM)
/// for the full rationale. Pass `1.0` for the raw metric.
pub fn select_level(
    chain: &LodChain,
    px_per_unit: f32,
    world_scale: f32,
    error_threshold_px: f32,
    error_conservatism: f32,
) -> usize {
    let mut chosen = 0;
    for (i, lvl) in chain.levels.iter().enumerate() {
        let projected = lvl.error * error_conservatism * world_scale * px_per_unit;
        if projected <= error_threshold_px {
            chosen = i + 1;
        } else {
            break;
        }
    }
    chosen
}

/// Camera distance at which a level with baked object-space `error` first fits
/// the screen-space budget — i.e. the distance beyond which [`select_level`]
/// accepts that level (its switch-ON distance). Inverse of the projection in
/// [`select_level`]: the level engages when
/// `error · conservatism · scale · px_per_unit(d) ≤ threshold`, so
/// `d ≥ error · conservatism · scale · (viewport_h/2) / (tan_half_fov_y · threshold)`.
///
/// Pure math shared with the tests that pin the calibration (see
/// `helmet_switch_distances_pin_the_calibration` below); the per-frame path
/// uses the [`select_level`] form directly.
pub fn switch_distance(
    error: f32,
    world_scale: f32,
    error_conservatism: f32,
    tan_half_fov_y: f32,
    viewport_h: f32,
    error_threshold_px: f32,
) -> f32 {
    if tan_half_fov_y <= 1e-6 || error_threshold_px <= 1e-6 {
        return f32::INFINITY;
    }
    error * error_conservatism * world_scale * (viewport_h * 0.5)
        / (tan_half_fov_y * error_threshold_px)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: usize) -> Vec<MeshKey> {
        let mut sm: slotmap::SlotMap<MeshKey, ()> = slotmap::SlotMap::with_key();
        (0..n).map(|_| sm.insert(())).collect()
    }

    fn chain(errors: &[f32], ks: &[MeshKey]) -> LodChain {
        LodChain {
            levels: errors
                .iter()
                .zip(ks)
                .map(|(&error, &mesh_key)| LodLevel { mesh_key, error })
                .collect(),
            bounds_radius: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn projection_scales_inversely_with_distance() {
        let near = projected_px_per_unit(1.0, 0.5, 1080.0);
        let far = projected_px_per_unit(10.0, 0.5, 1080.0);
        assert!((near / far - 10.0).abs() < 1e-3, "px/unit ∝ 1/distance");
        assert_eq!(projected_px_per_unit(0.0, 0.5, 1080.0), f32::INFINITY);
    }

    #[test]
    fn close_instance_picks_base() {
        let ks = keys(3);
        let c = chain(&[0.01, 0.05, 0.2], &ks);
        // Very high px/unit (instance fills the screen): even level 1's error
        // projects past a 1px budget → base.
        let level = select_level(&c, 10_000.0, 1.0, 1.0, 1.0);
        assert_eq!(level, 0);
    }

    #[test]
    fn far_instance_picks_coarsest() {
        let ks = keys(3);
        let c = chain(&[0.01, 0.05, 0.2], &ks);
        // Tiny px/unit (instance is a speck): every level is within a 1px budget
        // → coarsest.
        let level = select_level(&c, 1.0, 1.0, 1.0, 1.0);
        assert_eq!(level, 3);
        assert_eq!(c.key_for_level(ks[0], level), ks[2]);
    }

    #[test]
    fn mid_distance_picks_a_middle_level() {
        let ks = keys(3);
        let c = chain(&[0.01, 0.05, 0.2], &ks);
        // px/unit = 20: level errors project to 0.2, 1.0, 4.0 px. Budget 1px →
        // levels 1 and 2 pass (≤1), level 3 fails (4>1) → choose level 2.
        let level = select_level(&c, 20.0, 1.0, 1.0, 1.0);
        assert_eq!(level, 2);
        assert_eq!(c.key_for_level(ks[0], 0), ks[0]); // base
        assert_eq!(c.key_for_level(ks[0], 2), ks[1]);
    }

    #[test]
    fn larger_scale_biases_toward_finer_levels() {
        let ks = keys(3);
        let c = chain(&[0.01, 0.05, 0.2], &ks);
        // Same px/unit but the instance is scaled 5×: errors project 5× larger,
        // so a coarser level that passed now fails → a finer pick than at scale 1.
        let at_1 = select_level(&c, 20.0, 1.0, 1.0, 1.0);
        let at_5 = select_level(&c, 20.0, 5.0, 1.0, 1.0);
        assert!(at_5 <= at_1, "scaling up must not pick a coarser level");
        assert!(at_5 < at_1, "5× scale should pick strictly finer here");
    }

    #[test]
    fn conservatism_biases_toward_finer_levels() {
        let ks = keys(3);
        let c = chain(&[0.01, 0.05, 0.2], &ks);
        // px/unit = 20 with conservatism 16: errors project 3.2/16/64 px — all
        // past a 1px budget → base, where the raw metric picked level 2.
        assert_eq!(select_level(&c, 20.0, 1.0, 1.0, 1.0), 2);
        assert_eq!(select_level(&c, 20.0, 1.0, 1.0, 16.0), 0);
    }

    /// Pins the calibration against the DamagedHelmet bake (radius 1.27u,
    /// 15,452 base tris; level errors 3.97e-4/1.37e-3/4.61e-3 from the
    /// lod-bake's sqrt-QEM metric) at a 600px viewport with
    /// tan(fov_y/2)=0.4142 (≈45° vertical FOV).
    #[test]
    fn helmet_switch_distances_pin_the_calibration() {
        const HELMET_ERRORS: [f32; 3] = [3.97e-4, 1.37e-3, 4.61e-3];
        const HELMET_RADIUS: f32 = 1.27;
        const VIEWPORT_H: f32 = 600.0;
        const TAN_HALF_FOV: f32 = 0.4142;
        let threshold = crate::AwsmRenderer::LOD_ERROR_THRESHOLD_PX;
        let conservatism = crate::AwsmRenderer::LOD_ERROR_CONSERVATISM;

        // Regression documentation: with the raw metric (conservatism 1.0) the
        // level-1 switch distance was 0.29u — INSIDE the 1.27u-radius mesh, so
        // the base level could never draw from an exterior camera.
        let raw_d1 = switch_distance(
            HELMET_ERRORS[0],
            1.0,
            1.0,
            TAN_HALF_FOV,
            VIEWPORT_H,
            threshold,
        );
        assert!(
            (raw_d1 - 0.2876).abs() < 5e-3,
            "raw level-1 switch distance drifted: {raw_d1}"
        );
        assert!(raw_d1 < HELMET_RADIUS, "the bug this calibration fixes");

        // With the default calibration the switches step outward at sensible
        // object-relative ranges: 4.6u / 16u / 53u ≈ 3.6 / 12.6 / 42 radii.
        let d: Vec<f32> = HELMET_ERRORS
            .iter()
            .map(|&e| switch_distance(e, 1.0, conservatism, TAN_HALF_FOV, VIEWPORT_H, threshold))
            .collect();
        assert!(
            (d[0] - 4.60).abs() < 0.05 && (d[1] - 15.9).abs() < 0.2 && (d[2] - 53.4).abs() < 0.5,
            "calibrated switch distances drifted: {d:?}"
        );
        assert!(
            d[0] >= 3.5 * HELMET_RADIUS,
            "level 1 must not engage until the object is well outside inspect range: {} < 3.5 radii",
            d[0]
        );
        assert!(
            d[1] >= 2.0 * d[0] && d[2] >= 2.0 * d[1],
            "coarser levels step outward"
        );

        // The pure switch-distance form agrees with the per-frame select_level
        // form on both sides of the level-1 boundary.
        let ks = keys(3);
        let c = chain(&HELMET_ERRORS, &ks);
        let sel = |dist: f32| {
            let ppu = projected_px_per_unit(dist, TAN_HALF_FOV, VIEWPORT_H);
            select_level(&c, ppu, 1.0, threshold, conservatism)
        };
        assert_eq!(sel(d[0] * 0.99), 0, "just inside the switch → base mesh");
        assert_eq!(sel(d[0] * 1.01), 1, "just outside the switch → level 1");
        assert_eq!(sel(d[2] * 1.01), 3, "past the last switch → coarsest");
    }

    #[test]
    fn registry_round_trip_and_level_keys() {
        let ks = keys(3);
        let base = ks[0];
        let mut reg = LodRegistry::default();
        assert!(reg.is_empty());
        reg.register(base, chain(&[0.05, 0.2], &ks[1..]));
        assert!(!reg.is_empty());
        let got = reg.get(base).unwrap();
        assert_eq!(got.levels.len(), 2);
        let level_keys: Vec<_> = reg.level_keys().collect();
        assert_eq!(level_keys, vec![ks[1], ks[2]]);
        reg.clear();
        assert!(reg.is_empty());
    }
}
