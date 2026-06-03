//! Coarse-grained shadow-quality presets.
//!
//! Each tier is a flat preset over the renderer-wide `ShadowsConfig`
//! plus a per-light `LightShadowParams` template; callers pick a tier,
//! the preset table fills in every knob. `Custom` preserves the
//! per-knob authoring path for users who want full control.

use crate::shadows::{
    config::ShadowsConfig,
    light_shadow::{EvsmCutoff, LightShadowHardness, LightShadowParams},
};

/// Coarse-grained quality preset over the renderer's shadow knobs.
/// `Custom` opts out of the preset table — the editor surfaces every
/// `ShadowsConfig` / `LightShadowParams` knob directly. Switching from
/// a named tier back to `Custom` retains the in-memory values; the
/// editor sees them as the user's starting point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShadowQualityTier {
    /// Mobile-class. Tiny atlas, 2 cascades, no SSCS / EVSM.
    Low,
    /// Mid-range default. Reasonable atlas, 3 cascades, last-cascade EVSM.
    #[default]
    Medium,
    /// Desktop default. Full 4 cascades, two-cascade EVSM, SSCS on.
    High,
    /// Maximum quality. 8K atlas + 16 max point lights.
    Ultra,
    /// Bypass the preset table; consult the raw config / params.
    Custom,
}

/// Resolved knob set for a named tier. `apply_to_config` /
/// `apply_to_light_params` push the matching fields into the renderer
/// state without disturbing fields the tier doesn't own.
#[derive(Clone, Copy, Debug)]
pub struct ShadowQualityPreset {
    pub atlas_size: u32,
    pub cascade_count: u8,
    /// Filter mode applied to per-light shadows in this tier. Stored
    /// directly (rather than derived from a tap count) so the preset
    /// table is unambiguous about which mode High/Ultra actually use
    /// — earlier revisions stored a `pcf_taps: u32` and mapped it via
    /// a match in `apply_to_light_params`, but the 16-tap value the
    /// High/Ultra presets specified fell into the default-arm
    /// (leave-untouched) branch, leaving the field effectively dead.
    pub hardness: LightShadowHardness,
    pub max_point_shadows: u32,
    pub evsm_cutoff: EvsmCutoff,
    pub sscs_enabled: bool,
}

impl ShadowQualityTier {
    /// Returns the canonical preset for a named tier. `Custom` panics —
    /// callers must check `is_named` first or use `preset()` (which
    /// returns `None` for `Custom`).
    pub fn preset_unchecked(self) -> ShadowQualityPreset {
        match self {
            ShadowQualityTier::Low => ShadowQualityPreset {
                atlas_size: 1024,
                cascade_count: 2,
                hardness: LightShadowHardness::Soft,
                max_point_shadows: 2,
                evsm_cutoff: EvsmCutoff::Off,
                sscs_enabled: false,
            },
            ShadowQualityTier::Medium => ShadowQualityPreset {
                atlas_size: 2048,
                cascade_count: 3,
                hardness: LightShadowHardness::Soft,
                max_point_shadows: 4,
                evsm_cutoff: EvsmCutoff::LastCascade,
                sscs_enabled: false,
            },
            ShadowQualityTier::High => ShadowQualityPreset {
                atlas_size: 4096,
                cascade_count: 4,
                hardness: LightShadowHardness::Pcss,
                max_point_shadows: 8,
                evsm_cutoff: EvsmCutoff::LastTwoCascades,
                sscs_enabled: true,
            },
            ShadowQualityTier::Ultra => ShadowQualityPreset {
                atlas_size: 8192,
                cascade_count: 4,
                hardness: LightShadowHardness::Pcss,
                max_point_shadows: 16,
                evsm_cutoff: EvsmCutoff::LastTwoCascades,
                sscs_enabled: true,
            },
            ShadowQualityTier::Custom => {
                unreachable!("Custom tier has no preset; call .preset() and handle None")
            }
        }
    }

    /// Preset for a named tier, or `None` for `Custom`.
    pub fn preset(self) -> Option<ShadowQualityPreset> {
        match self {
            ShadowQualityTier::Custom => None,
            other => Some(other.preset_unchecked()),
        }
    }

    /// Whether this tier is one of the named presets (not Custom).
    pub fn is_named(self) -> bool {
        !matches!(self, ShadowQualityTier::Custom)
    }
}

impl ShadowQualityPreset {
    /// Applies the renderer-wide knobs to an existing `ShadowsConfig`.
    /// Knobs not owned by the preset (debug overlays, blur radius,
    /// EVSM exponent) are left alone — callers preserve their authored
    /// values across tier flips.
    pub fn apply_to_config(&self, config: &mut ShadowsConfig) {
        config.atlas_size = self.atlas_size;
        config.max_point_shadows = self.max_point_shadows;
        config.sscs_enabled = self.sscs_enabled;
    }

    /// Applies the per-light knobs the tier owns to an existing
    /// `LightShadowParams`. The `cast` flag is never touched — that's
    /// the caller's authored intent.
    pub fn apply_to_light_params(&self, params: &mut LightShadowParams) {
        params.cascade_count = self.cascade_count;
        params.evsm_cutoff = self.evsm_cutoff;
        params.hardness = self.hardness;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_tiers_have_strictly_growing_atlas_sizes() {
        let low = ShadowQualityTier::Low.preset_unchecked();
        let med = ShadowQualityTier::Medium.preset_unchecked();
        let high = ShadowQualityTier::High.preset_unchecked();
        let ultra = ShadowQualityTier::Ultra.preset_unchecked();
        assert!(low.atlas_size < med.atlas_size);
        assert!(med.atlas_size < high.atlas_size);
        assert!(high.atlas_size < ultra.atlas_size);
    }

    #[test]
    fn custom_has_no_preset() {
        assert!(ShadowQualityTier::Custom.preset().is_none());
        assert!(!ShadowQualityTier::Custom.is_named());
    }

    #[test]
    fn high_tier_application_preserves_cast() {
        let preset = ShadowQualityTier::High.preset_unchecked();
        let mut params = LightShadowParams {
            cast: true,
            ..LightShadowParams::default()
        };
        preset.apply_to_light_params(&mut params);
        assert!(params.cast, "tier application must not touch cast flag");
        assert_eq!(params.cascade_count, 4);
        assert_eq!(params.evsm_cutoff, EvsmCutoff::LastTwoCascades);
        assert_eq!(params.hardness, LightShadowHardness::Pcss);
    }

    #[test]
    fn low_and_medium_apply_soft_hardness() {
        for tier in [ShadowQualityTier::Low, ShadowQualityTier::Medium] {
            let preset = tier.preset_unchecked();
            let mut params = LightShadowParams::default();
            preset.apply_to_light_params(&mut params);
            assert_eq!(
                params.hardness,
                LightShadowHardness::Soft,
                "tier {tier:?} should apply Soft hardness"
            );
        }
    }
}
