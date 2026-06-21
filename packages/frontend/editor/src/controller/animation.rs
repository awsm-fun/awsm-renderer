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
//! Rotation is **quaternion-native**: a rotation
//! track's keyframe value is a `Quat` that lowers straight onto a `Quat` sampler
//! — there is no Euler representation in the model (Euler is only a UI projection).

use std::sync::Arc;

use awsm_renderer::animation::{
    AnimationChannel, AnimationData, AnimationSampler, AnimationTarget, TransformAnimation,
    VertexAnimation,
};
use awsm_web_shared::prelude::{Mutable, MutableVec};
use glam::{Quat, Vec2, Vec3, Vec4};

use crate::engine::scene::{AssetId, NodeId};

// The serde model types are the **persistence schema** (`awsm_editor_protocol`), so
// the live model + the project TOML share one definition (mirrors how
// `StoredMaterial` lives in scene-schema). Re-exported here so the rest of the
// editor (commands / query / bridge) references them via `controller::animation`.
pub use awsm_editor_protocol::animation::{
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
        TrackValue::Vec2(_) => TrackValue::Vec2([0.0; 2]),
        TrackValue::Vec3(_) => TrackValue::Vec3([0.0; 3]),
        TrackValue::Vec4(_) => TrackValue::Vec4([0.0; 4]),
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
/// Consumed by the Animation-mode UI.
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

/// The scene node a target binds to, if any. `Uniform` targets bind to a
/// material (by `AssetId`), not a node, so they return `None`.
pub fn target_node(t: &TrackTarget) -> Option<NodeId> {
    match t {
        TrackTarget::Transform { node, .. }
        | TrackTarget::Morph { node, .. }
        | TrackTarget::BuiltinParam { node, .. }
        | TrackTarget::Light { node, .. }
        | TrackTarget::Camera { node, .. } => Some(*node),
        TrackTarget::Uniform { .. } => None,
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
        // A morph track keys ONE scalar weight per keyframe (`TrackTarget::Morph`
        // + `TrackValue::Scalar`), but the renderer `Morph` target consumes the
        // **whole** weight vector (`AnimationData::Vertex { weights }`). Reconcile
        // by lowering each scalar into a weight vector of length `index + 1` whose
        // position `index` carries the keyed scalar and every other entry is 0.
        //
        // LIMITATION: this drives the morph at `index` correctly for the
        // common single-morph / first-target case, but because the lowered vector
        // forces all leading weights `0..index` to 0 every frame, a clip with
        // *separate* tracks for two morphs of the same mesh would have each track
        // stomp the other's weight. Per-index masked morph blending (write only
        // the keyed slot, leave the rest at rest) is deferred.
        let morph_index = match &self.target {
            TrackTarget::Morph { index, .. } => Some(*index),
            _ => None,
        };
        let to_data = |v: &TrackValue| -> AnimationData {
            match morph_index {
                Some(index) => morph_scalar_to_vertex(v, index),
                None => track_value_to_data(v, prop),
            }
        };
        let values: Vec<AnimationData> = keys.iter().map(|k| to_data(&k.value)).collect();

        let sampler = match self.sampler.get() {
            SamplerKind::Linear => AnimationSampler::new_linear(times, values),
            SamplerKind::Step => AnimationSampler::new_step(times, values),
            SamplerKind::Cubic => {
                let in_tangents: Vec<AnimationData> =
                    keys.iter().map(|k| to_data(&k.in_tangent)).collect();
                let out_tangents: Vec<AnimationData> =
                    keys.iter().map(|k| to_data(&k.out_tangent)).collect();
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

    /// The track's value at time `t`, interpolated from its keyframes so that
    /// inserting a key here doesn't change the curve. Linear for vec3/scalar,
    /// normalized-lerp for quaternions (an inserted key the user immediately
    /// tweaks doesn't warrant full step/cubic reconstruction). `None` if empty.
    pub fn sample_at(&self, t: f64) -> Option<TrackValue> {
        let times = self.times.get_cloned();
        let keys = self.keys.get_cloned();
        if times.is_empty() || keys.len() != times.len() {
            return None;
        }
        if t <= times[0] {
            return Some(keys[0].value);
        }
        let last = times.len() - 1;
        if t >= times[last] {
            return Some(keys[last].value);
        }
        let mut i = 0;
        while i + 1 < times.len() && times[i + 1] < t {
            i += 1;
        }
        let (t0, t1) = (times[i], times[i + 1]);
        let f = if t1 > t0 {
            ((t - t0) / (t1 - t0)) as f32
        } else {
            0.0
        };
        Some(lerp_value(&keys[i].value, &keys[i + 1].value, f))
    }
}

/// Linear interpolation between two track values (normalized-lerp for quats).
fn lerp_value(a: &TrackValue, b: &TrackValue, f: f32) -> TrackValue {
    match (a, b) {
        (TrackValue::Vec3(x), TrackValue::Vec3(y)) => TrackValue::Vec3([
            x[0] + (y[0] - x[0]) * f,
            x[1] + (y[1] - x[1]) * f,
            x[2] + (y[2] - x[2]) * f,
        ]),
        (TrackValue::Scalar(x), TrackValue::Scalar(y)) => TrackValue::Scalar(x + (y - x) * f),
        (TrackValue::Quat(x), TrackValue::Quat(y)) => {
            let mut r = [
                x[0] + (y[0] - x[0]) * f,
                x[1] + (y[1] - x[1]) * f,
                x[2] + (y[2] - x[2]) * f,
                x[3] + (y[3] - x[3]) * f,
            ];
            let n = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2] + r[3] * r[3]).sqrt();
            if n > 1e-6 {
                for c in &mut r {
                    *c /= n;
                }
            }
            TrackValue::Quat(r)
        }
        _ => *a,
    }
}

/// A sensible default value for a fresh keyframe on a track of this target —
/// identity rotation, unit scale, zero translation/scalar.
pub fn default_value_for(target: &TrackTarget) -> TrackValue {
    match target {
        TrackTarget::Transform {
            prop: TransformProp::Rotation,
            ..
        } => TrackValue::Quat([0.0, 0.0, 0.0, 1.0]),
        TrackTarget::Transform {
            prop: TransformProp::Scale,
            ..
        } => TrackValue::Vec3([1.0, 1.0, 1.0]),
        TrackTarget::Transform { .. } => TrackValue::Vec3([0.0; 3]),
        _ => TrackValue::Scalar(0.0),
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
/// track's own component set — so a rotation track leaves T/S untouched);
/// everything else lowers to the matching scalar/vec3/quat data.
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
        // Non-transform targets: scalar / vec2 / vec3 / vec4 / quat lower straight.
        (None, TrackValue::Scalar(s)) => AnimationData::F32(*s),
        (None, TrackValue::Vec2(v)) => AnimationData::Vec2(Vec2::from_array(*v)),
        (None, TrackValue::Vec3(v)) => AnimationData::Vec3(Vec3::from_array(*v)),
        (None, TrackValue::Vec4(v)) => AnimationData::Vec4(Vec4::from_array(*v)),
        (None, TrackValue::Quat(q)) => AnimationData::Quat(Quat::from_array(*q)),
        // Shape mismatch (e.g. a scalar value on a transform-rotation track): fall
        // back to inert data so lowering never panics. The lowering validation
        // (animation_sync) catches genuine mismatches as hard errors. vec2/vec4
        // never target a transform component, so they only reach here on mismatch.
        (Some(_), TrackValue::Scalar(s)) => AnimationData::F32(*s),
        (Some(_), TrackValue::Vec2(v)) => AnimationData::Vec2(Vec2::from_array(*v)),
        (Some(_), TrackValue::Vec4(v)) => AnimationData::Vec4(Vec4::from_array(*v)),
        (Some(TransformProp::Translation) | Some(TransformProp::Scale), TrackValue::Quat(q)) => {
            AnimationData::Quat(Quat::from_array(*q))
        }
        (Some(TransformProp::Rotation), TrackValue::Vec3(v)) => {
            AnimationData::Vec3(Vec3::from_array(*v))
        }
    }
}

/// Lower one authored morph keyframe (a single scalar weight) into the renderer
/// `AnimationData::Vertex` the morph target consumes: a weight vector of length
/// `index + 1` with position `index` carrying the scalar and the rest 0. A
/// non-scalar value (shape mismatch) lowers to its first component / 0. See the
/// reconciliation note + limitation in [`Track::lower`].
fn morph_scalar_to_vertex(value: &TrackValue, index: usize) -> AnimationData {
    let scalar = match value {
        TrackValue::Scalar(s) => *s,
        TrackValue::Vec2(v) => v.first().copied().unwrap_or(0.0),
        TrackValue::Vec3(v) => v.first().copied().unwrap_or(0.0),
        TrackValue::Vec4(v) => v.first().copied().unwrap_or(0.0),
        TrackValue::Quat(q) => q.first().copied().unwrap_or(0.0),
    };
    let mut weights = vec![0.0_f32; index + 1];
    weights[index] = scalar;
    AnimationData::Vertex(VertexAnimation::new(weights))
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
// live in `awsm_editor_protocol::animation` (re-exported above) so the live model +
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
// Transport + selection are controller state too: set via transient
// commands so they broadcast + snapshot but don't pollute undo. These are
// editor-runtime-only (not persisted in `EditorProject`). The data definitions
// now live in `awsm_editor_protocol` (so the MCP server shares them); re-exported
// here at their established path.
pub use awsm_editor_protocol::{AnimSel, AnimView, StepKind};
