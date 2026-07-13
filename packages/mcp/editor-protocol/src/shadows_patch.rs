//! Patch-merge for the renderer-wide [`ShadowsConfig`] — the pure,
//! host-testable core of the `SetShadows` command.
//!
//! Every field is `Option`: `None` PRESERVES the current value, `Some`
//! sets it (clamped to the renderer-legal range so a wild wire value
//! can't request an illegal GPU resource shape). The clamps mirror the
//! renderer's own (`shadows::consts` / `Shadows::set_config` /
//! `ShadowsConfig::EVSM_EXPONENT_MAX_FP16`) — this crate can't depend
//! on the renderer, so the bounds are restated here with their source
//! named; keep them in lockstep.

use awsm_renderer_scene::ShadowsConfig;

/// A partial update of the renderer-wide shadow config. `None` fields
/// preserve the current value. Built by the MCP `set_shadows` tool and
/// the editor's Shadows UI; applied via [`ShadowsPatch::apply`].
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ShadowsPatch {
    /// Master toggle for screen-space contact shadows (compile-time —
    /// flipping recompiles the shadow-consuming pipelines).
    #[serde(default)]
    pub sscs_enabled: Option<bool>,
    /// SSCS ray-march step count (compile-time loop bound; clamped ≥ 1).
    #[serde(default)]
    pub sscs_step_count: Option<u32>,
    /// World-space length of each SSCS step, in metres.
    #[serde(default)]
    pub sscs_step_world: Option<f32>,
    /// SSCS occluder-slab thickness in metres.
    #[serde(default)]
    pub sscs_thickness: Option<f32>,
    /// Max SSCS darkening for the directional shadow term (0..1).
    #[serde(default)]
    pub sscs_directional_darkening: Option<f32>,
    /// Max SSCS darkening for punctual (point/spot) shadow terms (0..1).
    #[serde(default)]
    pub sscs_punctual_darkening: Option<f32>,
    /// 2D PCF/spot atlas size in texels (square; rounded up to a power
    /// of two, clamped to 64..=8192 = `SHADOW_ATLAS_MAX_SIZE`).
    #[serde(default)]
    pub atlas_size: Option<u32>,
    /// EVSM moments atlas size in texels (square; rounded up to a power
    /// of two, clamped to 1..=8192 — 1 is legal for "never uses EVSM").
    #[serde(default)]
    pub evsm_atlas_size: Option<u32>,
    /// EVSM depth-warp exponent (clamped to 0.5..=18 =
    /// `EVSM_EXPONENT_MAX_FP16`; above that fp16 saturates and the
    /// visibility curve collapses to a binary mask).
    #[serde(default)]
    pub evsm_exponent: Option<f32>,
    /// EVSM Gaussian blur half-width in texels (clamped to 0..=8 =
    /// `evsm::MAX_BLUR_RADIUS`, the WGSL kernel-array bound).
    #[serde(default)]
    pub evsm_blur_radius: Option<u32>,
    /// Max simultaneous point-light shadow casters (cube-array slices;
    /// clamped to 1..=32 = `MAX_SHADOW_DESCRIPTORS`, the per-frame
    /// descriptor-UBO capacity).
    #[serde(default)]
    pub max_point_shadows: Option<u32>,
    /// Per-face cube shadow resolution in texels (square; rounded up to
    /// a power of two, clamped to 64..=8192 — mirrors
    /// `clamp_point_shadow_resolution`).
    #[serde(default)]
    pub point_shadow_resolution: Option<u32>,
    /// Tint each directional cascade range for authoring (live uniform).
    #[serde(default)]
    pub debug_cascade_colors: Option<bool>,
}

/// Round `v` up to the nearest power of two inside `[min, max]` (both
/// powers of two). Clamping BEFORE rounding keeps the result ≤ `max`.
fn pow2_clamp(v: u32, min: u32, max: u32) -> u32 {
    v.clamp(min, max).next_power_of_two()
}

impl ShadowsPatch {
    /// Merge this patch onto `base`: `None` preserves, `Some` sets
    /// (clamped — see the field docs for each bound and its renderer
    /// source constant).
    pub fn apply(&self, base: ShadowsConfig) -> ShadowsConfig {
        let mut cfg = base;
        if let Some(v) = self.sscs_enabled {
            cfg.sscs_enabled = v;
        }
        if let Some(v) = self.sscs_step_count {
            cfg.sscs_step_count = v.max(1);
        }
        if let Some(v) = self.sscs_step_world {
            cfg.sscs_step_world = v;
        }
        if let Some(v) = self.sscs_thickness {
            cfg.sscs_thickness = v;
        }
        if let Some(v) = self.sscs_directional_darkening {
            cfg.sscs_directional_darkening = v;
        }
        if let Some(v) = self.sscs_punctual_darkening {
            cfg.sscs_punctual_darkening = v;
        }
        if let Some(v) = self.atlas_size {
            cfg.atlas_size = pow2_clamp(v, 64, 8192);
        }
        if let Some(v) = self.evsm_atlas_size {
            cfg.evsm_atlas_size = pow2_clamp(v, 1, 8192);
        }
        if let Some(v) = self.evsm_exponent {
            cfg.evsm_exponent = v.clamp(0.5, 18.0);
        }
        if let Some(v) = self.evsm_blur_radius {
            cfg.evsm_blur_radius = v.min(8);
        }
        if let Some(v) = self.max_point_shadows {
            cfg.max_point_shadows = v.clamp(1, 32);
        }
        if let Some(v) = self.point_shadow_resolution {
            cfg.point_shadow_resolution = pow2_clamp(v, 64, 8192);
        }
        if let Some(v) = self.debug_cascade_colors {
            cfg.debug_cascade_colors = v;
        }
        cfg
    }

    /// A full-replace patch carrying every field of `cfg` — the undo
    /// inverse of any `SetShadows` patch.
    pub fn replace(cfg: &ShadowsConfig) -> Self {
        Self {
            sscs_enabled: Some(cfg.sscs_enabled),
            sscs_step_count: Some(cfg.sscs_step_count),
            sscs_step_world: Some(cfg.sscs_step_world),
            sscs_thickness: Some(cfg.sscs_thickness),
            sscs_directional_darkening: Some(cfg.sscs_directional_darkening),
            sscs_punctual_darkening: Some(cfg.sscs_punctual_darkening),
            atlas_size: Some(cfg.atlas_size),
            evsm_atlas_size: Some(cfg.evsm_atlas_size),
            evsm_exponent: Some(cfg.evsm_exponent),
            evsm_blur_radius: Some(cfg.evsm_blur_radius),
            max_point_shadows: Some(cfg.max_point_shadows),
            point_shadow_resolution: Some(cfg.point_shadow_resolution),
            debug_cascade_colors: Some(cfg.debug_cascade_colors),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_patch_preserves_everything() {
        let base = ShadowsConfig {
            atlas_size: 2048,
            evsm_blur_radius: 3,
            debug_cascade_colors: true,
            ..ShadowsConfig::default()
        };
        assert_eq!(ShadowsPatch::default().apply(base.clone()), base);
    }

    #[test]
    fn single_field_patch_changes_only_that_field() {
        let base = ShadowsConfig::default();
        let got = ShadowsPatch {
            evsm_blur_radius: Some(2),
            ..Default::default()
        }
        .apply(base.clone());
        assert_eq!(got.evsm_blur_radius, 2);
        assert_eq!(
            ShadowsConfig {
                evsm_blur_radius: base.evsm_blur_radius,
                ..got
            },
            base,
            "no other field may drift"
        );
    }

    #[test]
    fn resource_sizes_round_to_pow2_within_bounds() {
        let base = ShadowsConfig::default();
        let got = ShadowsPatch {
            atlas_size: Some(5000),           // → clamp → npo2 = 8192 (≤ cap)
            evsm_atlas_size: Some(1),         // 1 is legal ("never uses EVSM")
            point_shadow_resolution: Some(3), // → min 64
            ..Default::default()
        }
        .apply(base);
        assert_eq!(got.atlas_size, 8192);
        assert_eq!(got.evsm_atlas_size, 1);
        assert_eq!(got.point_shadow_resolution, 64);
    }

    #[test]
    fn scalar_clamps_mirror_the_renderer() {
        let got = ShadowsPatch {
            sscs_step_count: Some(0),      // ≥ 1
            evsm_exponent: Some(100.0),    // ≤ EVSM_EXPONENT_MAX_FP16
            evsm_blur_radius: Some(99),    // ≤ MAX_BLUR_RADIUS
            max_point_shadows: Some(1000), // ≤ MAX_SHADOW_DESCRIPTORS
            ..Default::default()
        }
        .apply(ShadowsConfig::default());
        assert_eq!(got.sscs_step_count, 1);
        assert_eq!(got.evsm_exponent, 18.0);
        assert_eq!(got.evsm_blur_radius, 8);
        assert_eq!(got.max_point_shadows, 32);
    }

    #[test]
    fn replace_round_trips_any_config() {
        let cfg = ShadowsConfig {
            sscs_enabled: true,
            sscs_step_count: 24,
            atlas_size: 2048,
            evsm_atlas_size: 4096,
            evsm_exponent: 12.0,
            evsm_blur_radius: 4,
            max_point_shadows: 4,
            point_shadow_resolution: 512,
            debug_cascade_colors: true,
            ..ShadowsConfig::default()
        };
        // Applying the full-replace patch of `cfg` onto anything yields `cfg`.
        assert_eq!(
            ShadowsPatch::replace(&cfg).apply(ShadowsConfig::default()),
            cfg
        );
    }

    #[test]
    fn deserializes_partial_json_with_missing_fields_as_none() {
        // Wire compat: an older sender that omits fields must patch only
        // what it names.
        let p: ShadowsPatch =
            serde_json::from_str(r#"{"debug_cascade_colors": true}"#).expect("partial json");
        assert_eq!(p.debug_cascade_colors, Some(true));
        assert_eq!(p.atlas_size, None);
        let empty: ShadowsPatch = serde_json::from_str("{}").expect("empty json");
        assert_eq!(empty, ShadowsPatch::default());
    }
}
