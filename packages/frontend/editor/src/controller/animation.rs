//! The reactive authored model for **animation clips** — the only thing the
//! Animation-mode studio authors. Mirrors `custom_material.rs` (`CustomMaterial`):
//! each clip is a registered asset with reactive fields the studio edits live,
//! plus a serde-serializable projection for persistence + the query surface.
//!
//! The authoring layer sits *above* the renderer runtime (`AnimationClipGroup` +
//! `AnimationMixer`): on every edit the clip **lowers** (auto-compiles, WYSIWYG)
//! into the renderer's animation system via the `animation_sync` bridge. The
//! lowering here is the pure, GPU-independent half — each [`Track`] builds a
//! renderer [`AnimationChannel`] given a resolver that maps the authored
//! [`TrackTarget`] descriptor → a live renderer [`AnimationTarget`] key.
//!
//! Rotation is **quaternion-native** (decision §10 / invariant I5): a rotation
//! track's keyframe value is a `Quat` that lowers straight onto a `Quat` sampler
//! — there is no Euler representation in the model (Euler is only a UI projection
//! added in M-A4).

use std::sync::Arc;

use awsm_renderer::animation::{
    AnimationChannel, AnimationData, AnimationSampler, AnimationTarget, TransformAnimation,
};
use awsm_web_shared::prelude::{Mutable, MutableVec};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::engine::scene::AssetId;

// The serde model types are the **persistence schema** (`awsm_scene_schema`), so
// the live model + the project TOML share one definition (mirrors how
// `StoredMaterial` lives in scene-schema). Re-exported here so the rest of the
// editor (commands / query / bridge) references them via `controller::animation`.
pub use awsm_scene_schema::animation::{
    BuiltinParamKind, CameraParamKind, ClipDirection, ClipLoop, Interp, Keyframe, LayerDoc,
    LayerModeDoc, LightParamKind, MixerDoc, SamplerKind, StoredAnimation, StoredTrack, StripDoc,
    TrackTarget, TrackValue, TransformProp,
};

/// The matching per-keyframe interp for a sampler kind (seeding fresh keyframes).
pub fn sampler_to_interp(kind: SamplerKind) -> Interp {
    match kind {
        SamplerKind::Step => Interp::Step,
        SamplerKind::Linear => Interp::Linear,
        SamplerKind::Cubic => Interp::Cubic,
    }
}

/// A zero value matching `value`'s shape (for default cubic tangents).
pub fn zeroed_like(value: &TrackValue) -> TrackValue {
    match value {
        TrackValue::Vec3(_) => TrackValue::Vec3([0.0; 3]),
        TrackValue::Quat(_) => TrackValue::Quat([0.0; 4]),
        TrackValue::Scalar(_) => TrackValue::Scalar(0.0),
    }
}

/// A fresh keyframe at `value` with the given interp and zeroed tangents.
pub fn new_keyframe(value: TrackValue, interp: Interp) -> Keyframe {
    let tan = zeroed_like(&value);
    Keyframe {
        value,
        interp,
        in_tangent: tan,
        out_tangent: tan,
    }
}

/// A short, stable identity string for a target (selection keys / dedupe).
/// Consumed by the Animation-mode UI (M-A2+).
#[allow(dead_code)]
pub fn target_key(t: &TrackTarget) -> String {
    match t {
        TrackTarget::Transform { node, prop } => format!("transform/{node}/{prop:?}"),
        TrackTarget::Morph { node, index } => format!("morph/{node}/{index}"),
        TrackTarget::Uniform { material, name } => format!("uniform/{material}/{name}"),
        TrackTarget::BuiltinParam { node, param } => format!("builtin/{node}/{param:?}"),
        TrackTarget::Light { node, param } => format!("light/{node}/{param:?}"),
        TrackTarget::Camera { node, param } => format!("camera/{node}/{param:?}"),
    }
}

/// One track: one object × one property, a single shared `times[]` (glTF-style)
/// + keyframes aligned to it. Mirrors a `CustomMaterial` field's reactivity.
pub struct Track {
    /// The serializable binding to a real target.
    pub target: TrackTarget,
    /// The sampler kind the whole track lowers to.
    pub sampler: Mutable<SamplerKind>,
    pub mute: Mutable<bool>,
    pub solo: Mutable<bool>,
    /// UI-only: whether the track row is expanded into per-channel lanes.
    pub expanded: Mutable<bool>,
    /// The **one shared** keyframe-time axis for this track (seconds).
    pub times: Mutable<Vec<f64>>,
    /// Keyframes aligned to `times` (`keys[i]` ↔ `times[i]`).
    pub keys: Mutable<Vec<Keyframe>>,
}

impl Track {
    /// A fresh, empty track bound to `target`.
    pub fn new(target: TrackTarget) -> Arc<Self> {
        let sampler = default_sampler_for(&target);
        Arc::new(Self {
            target,
            sampler: Mutable::new(sampler),
            mute: Mutable::new(false),
            solo: Mutable::new(false),
            expanded: Mutable::new(false),
            times: Mutable::new(Vec::new()),
            keys: Mutable::new(Vec::new()),
        })
    }

    /// Build the renderer [`AnimationChannel`] for this track, given a resolver
    /// that maps the authored [`TrackTarget`] → a live [`AnimationTarget`]. Returns
    /// `None` when the target is unresolved (pending/invalid — the caller decides)
    /// or the track has no keyframes.
    pub fn lower(
        &self,
        resolve: &impl Fn(&TrackTarget) -> Option<AnimationTarget>,
    ) -> Option<AnimationChannel> {
        if self.mute.get() {
            return None;
        }
        let target = resolve(&self.target)?;
        let times = self.times.get_cloned();
        let keys = self.keys.get_cloned();
        if times.is_empty() || keys.len() != times.len() {
            return None;
        }

        let prop = match &self.target {
            TrackTarget::Transform { prop, .. } => Some(*prop),
            _ => None,
        };
        let values: Vec<AnimationData> = keys
            .iter()
            .map(|k| track_value_to_data(&k.value, prop))
            .collect();

        let sampler = match self.sampler.get() {
            SamplerKind::Linear => AnimationSampler::new_linear(times, values),
            SamplerKind::Step => AnimationSampler::new_step(times, values),
            SamplerKind::Cubic => {
                let in_tangents: Vec<AnimationData> = keys
                    .iter()
                    .map(|k| track_value_to_data(&k.in_tangent, prop))
                    .collect();
                let out_tangents: Vec<AnimationData> = keys
                    .iter()
                    .map(|k| track_value_to_data(&k.out_tangent, prop))
                    .collect();
                AnimationSampler::new_cubic_spline(
                    self.times.get_cloned(),
                    values,
                    in_tangents,
                    out_tangents,
                )
            }
        };
        Some(AnimationChannel::new(target, sampler))
    }
}

/// The default sampler kind for a fresh track of the given target kind.
fn default_sampler_for(target: &TrackTarget) -> SamplerKind {
    match target {
        // Step makes a fresh morph/uniform read crisply; everything else linear.
        TrackTarget::Transform { .. } => SamplerKind::Linear,
        _ => SamplerKind::Linear,
    }
}

/// Lower one authored [`TrackValue`] → the renderer [`AnimationData`] for a
/// channel. Transform tracks lower to a per-field `TransformAnimation` (only the
/// track's own component set — so a rotation track leaves T/S untouched, invariant
/// I3); everything else lowers to the matching scalar/vec3/quat data.
fn track_value_to_data(value: &TrackValue, prop: Option<TransformProp>) -> AnimationData {
    match (prop, value) {
        (Some(TransformProp::Translation), TrackValue::Vec3(v)) => {
            AnimationData::Transform(TransformAnimation::new_translation(Vec3::from_array(*v)))
        }
        (Some(TransformProp::Scale), TrackValue::Vec3(v)) => {
            AnimationData::Transform(TransformAnimation::new_scale(Vec3::from_array(*v)))
        }
        (Some(TransformProp::Rotation), TrackValue::Quat(q)) => {
            AnimationData::Transform(TransformAnimation::new_rotation(Quat::from_array(*q)))
        }
        // Non-transform targets: scalar / vec3 / quat lower straight.
        (None, TrackValue::Scalar(s)) => AnimationData::F32(*s),
        (None, TrackValue::Vec3(v)) => AnimationData::Vec3(Vec3::from_array(*v)),
        (None, TrackValue::Quat(q)) => AnimationData::Quat(Quat::from_array(*q)),
        // Shape mismatch (e.g. a scalar value on a transform-rotation track): fall
        // back to an inert F32 0 so lowering never panics. The lowering validation
        // (animation_sync) catches genuine mismatches as hard errors.
        (Some(_), TrackValue::Scalar(s)) => AnimationData::F32(*s),
        (Some(TransformProp::Translation) | Some(TransformProp::Scale), TrackValue::Quat(q)) => {
            AnimationData::Quat(Quat::from_array(*q))
        }
        (Some(TransformProp::Rotation), TrackValue::Vec3(v)) => {
            AnimationData::Vec3(Vec3::from_array(*v))
        }
    }
}

/// A live, reactive animation clip in the library
/// (`EditorController::custom_animations`). Mirrors [`CustomMaterial`].
///
/// [`CustomMaterial`]: super::custom_material::CustomMaterial
pub struct CustomAnimation {
    pub id: AssetId,
    pub name: Mutable<String>,
    /// Clip duration in seconds.
    pub duration: Mutable<f64>,
    pub loop_style: Mutable<ClipLoop>,
    /// Playback speed multiplier (1.0 = authored speed).
    pub speed: Mutable<f64>,
    pub direction: Mutable<ClipDirection>,
    /// Per-clip color as a `#rrggbb` hex string (library swatch).
    pub color: Mutable<String>,
    pub tracks: MutableVec<Arc<Track>>,
}

impl CustomAnimation {
    pub fn new(id: AssetId, name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            id,
            name: Mutable::new(name.into()),
            duration: Mutable::new(2.0),
            loop_style: Mutable::new(ClipLoop::Loop),
            speed: Mutable::new(1.0),
            direction: Mutable::new(ClipDirection::Forward),
            color: Mutable::new("#7aa2f7".to_string()),
            tracks: MutableVec::new(),
        })
    }
}

/// Find a clip in the live library by id.
pub fn find_clip(
    clips: &MutableVec<Arc<CustomAnimation>>,
    id: AssetId,
) -> Option<Arc<CustomAnimation>> {
    clips.lock_ref().iter().find(|c| c.id == id).map(Arc::clone)
}

// ───────────────────────────── serde projection ─────────────────────────────
// The stored projection types (`StoredAnimation`, `StoredTrack`, the mixer docs)
// live in `awsm_scene_schema::animation` (re-exported above) so the live model +
// the project TOML share one definition. The conversions to/from the live model
// are free functions here (the stored types are foreign).

/// Rebuild a live [`Track`] from a [`StoredTrack`].
pub fn stored_track_to_live(t: &StoredTrack) -> Arc<Track> {
    Arc::new(Track {
        target: t.target.clone(),
        sampler: Mutable::new(t.sampler),
        mute: Mutable::new(t.mute),
        solo: Mutable::new(t.solo),
        expanded: Mutable::new(t.expanded),
        times: Mutable::new(t.times.clone()),
        keys: Mutable::new(t.keys.clone()),
    })
}

/// Snapshot a live track into its serializable [`StoredTrack`] form.
pub fn stored_track_from_live(t: &Track) -> StoredTrack {
    StoredTrack {
        target: t.target.clone(),
        sampler: t.sampler.get(),
        mute: t.mute.get(),
        solo: t.solo.get(),
        expanded: t.expanded.get(),
        times: t.times.get_cloned(),
        keys: t.keys.get_cloned(),
    }
}

/// Snapshot a live clip into its serializable [`StoredAnimation`] form.
pub fn stored_from_live(c: &CustomAnimation) -> StoredAnimation {
    StoredAnimation {
        id: c.id,
        name: c.name.get_cloned(),
        duration: c.duration.get(),
        loop_style: c.loop_style.get(),
        speed: c.speed.get(),
        direction: c.direction.get(),
        color: c.color.get_cloned(),
        tracks: c
            .tracks
            .lock_ref()
            .iter()
            .map(|t| stored_track_from_live(t))
            .collect(),
    }
}

/// Rebuild a live clip from a [`StoredAnimation`] (same id, so node/material refs
/// resolve).
pub fn stored_to_live(s: &StoredAnimation) -> Arc<CustomAnimation> {
    let tracks: Vec<Arc<Track>> = s.tracks.iter().map(stored_track_to_live).collect();
    Arc::new(CustomAnimation {
        id: s.id,
        name: Mutable::new(s.name.clone()),
        duration: Mutable::new(s.duration),
        loop_style: Mutable::new(s.loop_style),
        speed: Mutable::new(s.speed),
        direction: Mutable::new(s.direction),
        color: Mutable::new(if s.color.is_empty() {
            "#7aa2f7".to_string()
        } else {
            s.color.clone()
        }),
        tracks: MutableVec::new_with_values(tracks),
    })
}

// ──────────────────────── controller transport / selection state ────────────
// Transport + selection are controller state too (§0.2): set via transient
// commands so they broadcast + snapshot but don't pollute undo. These are
// editor-runtime-only (not persisted in `EditorProject`), so they live here.

/// Which timeline editor the dock shows. Controller state (so synced tabs agree),
/// not pure view chrome.
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
