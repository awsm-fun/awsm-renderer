//! Wire vocabulary of [`crate::EditorCommand::SetAntiAliasing`]: the AA mode,
//! the TAA presets and debug views, and a patch of the TAA tuning.
//!
//! The persisted AA choice is `scene.post_process.anti_aliasing`
//! ([`awsm_renderer_scene::AntiAliasingConfig`]); these types are how a
//! command addresses it.

use awsm_renderer_scene::TaaTuningSettings;

/// The anti-aliasing mode. MSAA, SMAA and TAA are independent modes; the one
/// combination that predates TAA (MSAA with SMAA on top) stays selectable so
/// every existing scene's choice can be represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum AntiAliasingMode {
    /// No anti-aliasing.
    None,
    /// 4× multisampling.
    Msaa,
    /// SMAA post-process anti-aliasing alone.
    Smaa,
    /// 4× multisampling with SMAA on top (the pre-TAA combination).
    MsaaSmaa,
    /// Temporal anti-aliasing (and temporal upscaling below render scale 1).
    Taa,
}

impl AntiAliasingMode {
    /// The mode an authored config selects.
    pub fn of(aa: &awsm_renderer_scene::AntiAliasingConfig) -> Self {
        match (aa.taa.is_some(), aa.msaa, aa.smaa) {
            (true, _, _) => Self::Taa,
            (false, true, true) => Self::MsaaSmaa,
            (false, true, false) => Self::Msaa,
            (false, false, true) => Self::Smaa,
            (false, false, false) => Self::None,
        }
    }

    /// `(msaa, smaa)` of a non-TAA mode (`(false, false)` for TAA).
    pub fn msaa_smaa(self) -> (bool, bool) {
        match self {
            Self::None | Self::Taa => (false, false),
            Self::Msaa => (true, false),
            Self::Smaa => (false, true),
            Self::MsaaSmaa => (true, true),
        }
    }

    /// The wire name (`"none"`, `"msaa"`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Msaa => "msaa",
            Self::Smaa => "smaa",
            Self::MsaaSmaa => "msaa_smaa",
            Self::Taa => "taa",
        }
    }
}

/// TAA render-scale presets (render size / output size).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TaaPresetName {
    /// 1.0: anti-aliasing only, no upscaling, no sharpening.
    Native,
    /// 0.67 of the width and height (45% of the pixels).
    Quality,
    /// 0.58 (34% of the pixels).
    Balanced,
    /// 0.5 (25% of the pixels).
    Performance,
}

/// TAA debug views (transient editor view state, never persisted). Mirrors
/// `awsm_renderer::taa::TaaDebugView`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum TaaDebugViewName {
    /// Normal output.
    #[default]
    None,
    /// Motion vectors: hue = direction, brightness = length.
    Velocity,
    /// |current − previous frame warped by the vector|, heatmap.
    ReprojectionError,
    /// History length `n / N_max`, black to white.
    HistoryLength,
    /// R = off-screen reset, G = disocclusion, B = clip amount.
    Rejection,
    /// The reactive mask.
    Reactive,
    /// Upscaling only: weight of the nearest current-frame sample.
    Confidence,
    /// Normal output with the camera jitter held at zero.
    FreezeJitter,
    /// Only the (clipped) history side of the blend.
    HistoryOnly,
    /// Only the current frame's reconstruction.
    CurrentOnly,
}

impl TaaDebugViewName {
    /// Every view, in menu order.
    pub const ALL: [TaaDebugViewName; 10] = [
        Self::None,
        Self::Velocity,
        Self::ReprojectionError,
        Self::HistoryLength,
        Self::Rejection,
        Self::Reactive,
        Self::Confidence,
        Self::FreezeJitter,
        Self::HistoryOnly,
        Self::CurrentOnly,
    ];

    /// The wire name (`"history_length"`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Velocity => "velocity",
            Self::ReprojectionError => "reprojection_error",
            Self::HistoryLength => "history_length",
            Self::Rejection => "rejection",
            Self::Reactive => "reactive",
            Self::Confidence => "confidence",
            Self::FreezeJitter => "freeze_jitter",
            Self::HistoryOnly => "history_only",
            Self::CurrentOnly => "current_only",
        }
    }
}

/// A patch of the TAA tuning: only the `Some` fields change. Every field is
/// a live uniform (no recompile, no history reset).
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TaaTuningPatch {
    #[serde(default)]
    pub gamma_still: Option<f32>,
    #[serde(default)]
    pub gamma_moving: Option<f32>,
    #[serde(default)]
    pub gamma_moving_speed: Option<f32>,
    #[serde(default)]
    pub disocclusion_threshold: Option<f32>,
    #[serde(default)]
    pub max_history_native: Option<f32>,
    #[serde(default)]
    pub max_history_upscaling: Option<f32>,
    #[serde(default)]
    pub motion_alpha: Option<f32>,
    #[serde(default)]
    pub motion_alpha_speed: Option<f32>,
    #[serde(default)]
    pub reactive_transparent: Option<f32>,
    #[serde(default)]
    pub reactive_material: Option<f32>,
    #[serde(default)]
    pub reactive_particles: Option<f32>,
    #[serde(default)]
    pub reactive_lines: Option<f32>,
    #[serde(default)]
    pub edge_release: Option<f32>,
    #[serde(default)]
    pub reactive_new_objects: Option<f32>,
    #[serde(default)]
    pub reactive_ssr: Option<f32>,
    #[serde(default)]
    pub mip_bias_extra: Option<f32>,
    #[serde(default)]
    pub max_history_moving: Option<f32>,
    #[serde(default)]
    pub max_history_moving_speed: Option<f32>,
    #[serde(default)]
    pub motion_edge_alpha: Option<f32>,
    #[serde(default)]
    pub motion_edge_speed: Option<f32>,
}

impl TaaTuningPatch {
    /// A patch that sets every field to `t` (an undo inverse).
    pub fn replace(t: &TaaTuningSettings) -> Self {
        Self {
            gamma_still: Some(t.gamma_still),
            gamma_moving: Some(t.gamma_moving),
            gamma_moving_speed: Some(t.gamma_moving_speed),
            disocclusion_threshold: Some(t.disocclusion_threshold),
            max_history_native: Some(t.max_history_native),
            max_history_upscaling: Some(t.max_history_upscaling),
            motion_alpha: Some(t.motion_alpha),
            motion_alpha_speed: Some(t.motion_alpha_speed),
            reactive_transparent: Some(t.reactive_transparent),
            reactive_material: Some(t.reactive_material),
            reactive_particles: Some(t.reactive_particles),
            reactive_lines: Some(t.reactive_lines),
            edge_release: Some(t.edge_release),
            reactive_new_objects: Some(t.reactive_new_objects),
            reactive_ssr: Some(t.reactive_ssr),
            mip_bias_extra: Some(t.mip_bias_extra),
            max_history_moving: Some(t.max_history_moving),
            max_history_moving_speed: Some(t.max_history_moving_speed),
            motion_edge_alpha: Some(t.motion_edge_alpha),
            motion_edge_speed: Some(t.motion_edge_speed),
        }
    }

    /// Applies the `Some` fields to `t`. Non-finite values are ignored.
    pub fn apply(&self, t: &mut TaaTuningSettings) {
        let set = |dst: &mut f32, v: Option<f32>| {
            if let Some(v) = v.filter(|v| v.is_finite()) {
                *dst = v;
            }
        };
        set(&mut t.gamma_still, self.gamma_still);
        set(&mut t.gamma_moving, self.gamma_moving);
        set(&mut t.gamma_moving_speed, self.gamma_moving_speed);
        set(&mut t.disocclusion_threshold, self.disocclusion_threshold);
        set(&mut t.max_history_native, self.max_history_native);
        set(&mut t.max_history_upscaling, self.max_history_upscaling);
        set(&mut t.motion_alpha, self.motion_alpha);
        set(&mut t.motion_alpha_speed, self.motion_alpha_speed);
        set(&mut t.reactive_transparent, self.reactive_transparent);
        set(&mut t.reactive_material, self.reactive_material);
        set(&mut t.reactive_particles, self.reactive_particles);
        set(&mut t.reactive_lines, self.reactive_lines);
        set(&mut t.edge_release, self.edge_release);
        set(&mut t.reactive_new_objects, self.reactive_new_objects);
        set(&mut t.reactive_ssr, self.reactive_ssr);
        set(&mut t.mip_bias_extra, self.mip_bias_extra);
        set(&mut t.max_history_moving, self.max_history_moving);
        set(
            &mut t.max_history_moving_speed,
            self.max_history_moving_speed,
        );
        set(&mut t.motion_edge_alpha, self.motion_edge_alpha);
        set(&mut t.motion_edge_speed, self.motion_edge_speed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awsm_renderer_scene::{AntiAliasingConfig, TaaSettings};

    #[test]
    fn mode_round_trips_through_the_config() {
        for mode in [
            AntiAliasingMode::None,
            AntiAliasingMode::Msaa,
            AntiAliasingMode::Smaa,
            AntiAliasingMode::MsaaSmaa,
        ] {
            let (msaa, smaa) = mode.msaa_smaa();
            let cfg = AntiAliasingConfig {
                msaa,
                smaa,
                taa: None,
            };
            assert_eq!(AntiAliasingMode::of(&cfg), mode);
        }
        let taa = AntiAliasingConfig {
            msaa: false,
            smaa: false,
            taa: Some(TaaSettings::default()),
        };
        assert_eq!(AntiAliasingMode::of(&taa), AntiAliasingMode::Taa);
    }

    #[test]
    fn tuning_patch_replace_then_apply_is_identity() {
        let mut t = TaaTuningSettings {
            gamma_moving: 0.6,
            ..TaaTuningSettings::default()
        };
        let snapshot = t;
        TaaTuningPatch::replace(&snapshot).apply(&mut t);
        assert_eq!(t, snapshot);
        TaaTuningPatch {
            motion_alpha: Some(0.3),
            gamma_still: Some(f32::NAN),
            ..Default::default()
        }
        .apply(&mut t);
        assert_eq!(t.motion_alpha, 0.3);
        assert_eq!(t.gamma_still, snapshot.gamma_still, "NaN is ignored");
    }

    /// `replace` + `apply` carry EVERY tuning field: each one is moved off its
    /// default through serde, so a field added to the schema but not to the
    /// patch fails here without being listed.
    #[test]
    fn tuning_patch_covers_every_field() {
        let mut json = serde_json::to_value(TaaTuningSettings::default()).unwrap();
        let fields = json.as_object().unwrap().len();
        for (i, v) in json.as_object_mut().unwrap().values_mut().enumerate() {
            *v = serde_json::json!(v.as_f64().unwrap() + 0.125 * (i as f64 + 1.0));
        }
        let authored: TaaTuningSettings = serde_json::from_value(json).unwrap();
        let mut t = TaaTuningSettings::default();
        TaaTuningPatch::replace(&authored).apply(&mut t);
        assert_eq!(t, authored);
        let patch = serde_json::to_value(TaaTuningPatch::default()).unwrap();
        assert_eq!(
            patch.as_object().unwrap().len(),
            fields,
            "one patch field per tuning field"
        );
    }

    /// `set_anti_aliasing` carries the disocclusion level by its schema
    /// name, and leaves it unchanged when absent.
    #[test]
    fn set_anti_aliasing_carries_the_disocclusion_level() {
        use crate::EditorCommand;
        use awsm_renderer_scene::TaaDisocclusionSetting;
        for level in TaaDisocclusionSetting::ALL {
            let cmd: EditorCommand = serde_json::from_value(serde_json::json!({
                "cmd": "set_anti_aliasing",
                "taa_disocclusion": level.as_str(),
            }))
            .unwrap();
            match cmd {
                EditorCommand::SetAntiAliasing {
                    taa_disocclusion, ..
                } => assert_eq!(taa_disocclusion, Some(level)),
                other => panic!("{other:?}"),
            }
        }
        let cmd: EditorCommand =
            serde_json::from_value(serde_json::json!({ "cmd": "set_anti_aliasing" })).unwrap();
        assert!(matches!(
            cmd,
            EditorCommand::SetAntiAliasing {
                taa_disocclusion: None,
                taa_bloom_source: None,
                ..
            }
        ));
        assert!(serde_json::from_value::<EditorCommand>(serde_json::json!({
            "cmd": "set_anti_aliasing",
            "taa_disocclusion": "plane",
        }))
        .is_err());
    }
}
