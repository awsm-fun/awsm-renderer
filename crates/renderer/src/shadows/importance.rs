//! Importance-based per-light shadow budgets (Cluster 4.2).
//!
//! The heuristic is the plan's `contribution = bounds_overlap_with_camera_frustum *
//! intensity / (1 + distance_squared)`. The resulting score maps each
//! shadow-casting light to a coarse tier and then to a preset table
//! that scales `resolution`, `cascade_count`, and (for point lights)
//! `cube_face_update_rate`. Directional lights get a fixed High tier
//! — they affect the whole scene and quality scaling them is wrong.
//!
//! Lands materially better with `SceneSpatial` in place because the
//! cheap "is this light's bounds even in the camera frustum?" check
//! becomes a single `query_envelope` plus a frustum predicate, not a
//! full per-frame walk of every mesh.

use glam::Vec3;

use crate::{
    frustum::Frustum,
    lights::{Light, LightKey},
    shadows::{
        light_shadow::{CubeFaceUpdateRate, LightShadowParams},
        quality_tier::ShadowQualityTier,
    },
    AwsmRenderer,
};

/// Decision a single light gets from the importance pass.
#[derive(Clone, Copy, Debug)]
pub struct LightImportanceDecision {
    pub tier: ShadowQualityTier,
    pub resolution: u32,
    pub cube_face_update_rate: CubeFaceUpdateRate,
}

impl LightImportanceDecision {
    /// Resolution preset for a tier. Sized so the atlas (per Cluster
    /// 4.1's preset) has room for the typical light count at this tier.
    pub fn resolution_for_tier(tier: ShadowQualityTier) -> u32 {
        match tier {
            ShadowQualityTier::Low => 256,
            ShadowQualityTier::Medium => 512,
            ShadowQualityTier::High => 1024,
            ShadowQualityTier::Ultra => 2048,
            ShadowQualityTier::Custom => 1024,
        }
    }
}

impl AwsmRenderer {
    /// Walks every shadow-casting light and updates its
    /// `LightShadowParams` to the tier its importance score earns this
    /// frame. Off-screen lights drop to Low; lights filling the screen
    /// climb to Ultra.
    ///
    /// Call this on a slow tick — the importance heuristic is a coarse
    /// signal and re-running it every frame just churns the shadow
    /// allocator. Once every 10–30 frames is plenty.
    pub fn refresh_light_importance_budgets(&mut self) {
        let Some(matrices) = self.camera.last_matrices.as_ref() else {
            return;
        };
        // World-space camera position = translation column of the inverse view.
        // The earlier `.transpose()` here read the bottom row of `view.inverse()`,
        // which is (0,0,0,1) for any affine view → camera_pos was effectively
        // (0,0,0) regardless of camera. That broke the `distance_squared` term
        // in `light_importance_decision`, so importance scores treated every
        // light as if the camera were at the origin.
        let camera_pos = matrices.view.inverse().w_axis.truncate();
        let frustum = Frustum::from_view_projection(matrices.view_projection());

        // Snapshot the light keys + state so we can mutate
        // `shadows.params` without holding a borrow on `self.lights`.
        let snapshot: Vec<(LightKey, Light)> = self
            .lights
            .iter()
            .map(|(k, l)| (k, l.clone()))
            .collect();

        for (light_key, light) in snapshot {
            // Skip if the light doesn't cast shadows.
            let casts = self
                .shadows
                .light_params(light_key)
                .map(|p| p.cast)
                .unwrap_or(false);
            if !casts {
                continue;
            }
            let decision = light_importance_decision(&light, camera_pos, &frustum);
            if let Some(params) = self.shadows.params.get_mut(light_key) {
                apply_decision(params, decision);
            }
            self.lights.mark_punctual_dirty();
        }
    }
}

fn apply_decision(params: &mut LightShadowParams, decision: LightImportanceDecision) {
    params.resolution = decision.resolution;
    params.cube_face_update_rate = decision.cube_face_update_rate;
    if let Some(preset) = decision.tier.preset() {
        preset.apply_to_light_params(params);
    }
}

fn light_importance_decision(
    light: &Light,
    camera_pos: Vec3,
    camera_frustum: &Frustum,
) -> LightImportanceDecision {
    // Directional lights are global; pin them to High and update every
    // frame. Tier scaling a directional looks wrong — it lights every
    // mesh equally regardless of camera pose.
    if matches!(light, Light::Directional { .. }) {
        return LightImportanceDecision {
            tier: ShadowQualityTier::High,
            resolution: 2048,
            cube_face_update_rate: CubeFaceUpdateRate::EveryFrame,
        };
    }

    let Some(aabb) = light.world_aabb() else {
        return LightImportanceDecision {
            tier: ShadowQualityTier::Low,
            resolution: LightImportanceDecision::resolution_for_tier(ShadowQualityTier::Low),
            cube_face_update_rate: CubeFaceUpdateRate::Every2Frames,
        };
    };

    // Off-screen → Low. Cheap test against the camera frustum.
    let in_frustum = camera_frustum.intersects_aabb(&aabb);
    if !in_frustum {
        return LightImportanceDecision {
            tier: ShadowQualityTier::Low,
            resolution: LightImportanceDecision::resolution_for_tier(ShadowQualityTier::Low),
            cube_face_update_rate: CubeFaceUpdateRate::Every2Frames,
        };
    }

    let (position, intensity) = match light {
        Light::Point {
            position,
            intensity,
            ..
        } => (Vec3::from(*position), *intensity),
        Light::Spot {
            position,
            intensity,
            ..
        } => (Vec3::from(*position), *intensity),
        Light::Directional { .. } => unreachable!("directional handled above"),
    };

    let dist_sq = (position - camera_pos).length_squared().max(0.001);
    let score = intensity / (1.0 + dist_sq);

    // Cutoffs — re-tuned against `tuning-importance-tiers` (plan
    // §15 row T3). 4×4 (distance × intensity) grid: distances
    // {1, 5, 15, 50} m, intensities {1, 10, 100, 1000}. With the
    // old (0.1 / 1.0 / 4.0) cutoffs the distribution was 7 / 4 / 1 / 4
    // — almost nothing in High because the [1, 4] band is narrow
    // in score space. The current (0.05 / 1.0 / 10.0) cutoffs give
    // 6 / 5 / 3 / 2 on the same scene: a more even spread that
    // matches "most lights are minor, hero-bright + close gets
    // Ultra, mid-range bright gets High." Games tuning their own
    // content should override per-light tiers explicitly; these
    // cutoffs are the *default* heuristic.
    let tier = if score > 10.0 {
        ShadowQualityTier::Ultra
    } else if score > 1.0 {
        ShadowQualityTier::High
    } else if score > 0.05 {
        ShadowQualityTier::Medium
    } else {
        ShadowQualityTier::Low
    };

    let resolution = LightImportanceDecision::resolution_for_tier(tier);
    let cube_face_update_rate = match tier {
        ShadowQualityTier::Low => CubeFaceUpdateRate::Every2Frames,
        _ => CubeFaceUpdateRate::EveryFrame,
    };

    LightImportanceDecision {
        tier,
        resolution,
        cube_face_update_rate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directional_pins_to_high() {
        let light = Light::Directional {
            color: [1.0, 1.0, 1.0],
            intensity: 1.0,
            direction: [0.0, -1.0, 0.0],
        };
        let frustum =
            Frustum::from_view_projection(glam::Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0));
        let d = light_importance_decision(&light, Vec3::ZERO, &frustum);
        assert_eq!(d.tier, ShadowQualityTier::High);
    }

    #[test]
    fn out_of_frustum_point_drops_to_low() {
        // Camera looks down +Z; light is behind at -Z = -100.
        let view = glam::Mat4::look_at_rh(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0), Vec3::Y);
        let proj = glam::Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.1, 50.0);
        let frustum = Frustum::from_view_projection(proj * view);
        let light = Light::Point {
            color: [1.0; 3],
            intensity: 10.0,
            position: [0.0, 0.0, -100.0],
            range: 1.0,
        };
        let d = light_importance_decision(&light, Vec3::ZERO, &frustum);
        assert_eq!(d.tier, ShadowQualityTier::Low);
    }

    #[test]
    fn close_strong_point_climbs_to_ultra() {
        let view = glam::Mat4::look_at_rh(Vec3::ZERO, Vec3::new(0.0, 0.0, 1.0), Vec3::Y);
        let proj = glam::Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.1, 50.0);
        let frustum = Frustum::from_view_projection(proj * view);
        let light = Light::Point {
            color: [1.0; 3],
            intensity: 100.0,
            position: [0.0, 0.0, 1.0],
            range: 5.0,
        };
        let d = light_importance_decision(&light, Vec3::ZERO, &frustum);
        assert_eq!(d.tier, ShadowQualityTier::Ultra);
    }
}
