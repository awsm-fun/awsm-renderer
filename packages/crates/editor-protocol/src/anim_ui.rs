//! Editor-only animation UI enums (timeline selection / view / transport step).
//! These are control-layer state, not part of the persisted scene schema — the
//! scene-schema animation *data* types (`TrackTarget`, `Keyframe`, `MixerDoc`, …)
//! live in `awsm_scene::animation`.

use serde::{Deserialize, Serialize};

/// Which timeline editor the Animation-mode dock shows. Persisted view chrome,
/// not pure ephemeral UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimView {
    #[default]
    Dope,
    Curves,
    Mixer,
}

/// A step-playhead direction (transport buttons).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// Jump to clip start (t = 0).
    Home,
    /// Previous keyframe (of the selected/active track).
    Prev,
    /// Next keyframe.
    Next,
    /// Jump to clip end (t = duration).
    End,
}

/// The selected timeline element (track / keyframe). Identified by track index
/// within the active clip + optional keyframe index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimSel {
    /// Index of the selected track in the active clip's `tracks`.
    pub track: usize,
    /// The selected keyframe within that track, if any.
    #[serde(default)]
    pub keyframe: Option<usize>,
}
