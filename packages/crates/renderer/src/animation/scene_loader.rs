//! Reusable loader: turn editor-authored `awsm_scene_schema` animation data into
//! the renderer's runtime [`AnimationClipGroup`] / [`AnimationMixer`].
//!
//! This is the missing piece that lets a **game** (not just the editor) play
//! animations authored in `awsm-editor`. The editor's own lowering lived in the
//! editor crate; the *pure data* part of it lives here, behind the
//! `scene-schema` feature, so any consumer can use it.
//!
//! The opinionated parts — how an abstract `TrackTarget` (a node-id / asset-id +
//! property) maps to a concrete renderer [`AnimationTarget`], which inserted
//! [`AnimationClipKey`] a clip `AssetId` refers to, and how a layer's node mask
//! resolves to a [`TargetMask`] — are **caller-provided closures**. The lowering
//! here is pure.
//!
//! Typical game flow:
//! ```ignore
//! let project: awsm_scene_schema::EditorProject = toml::from_str(&text)?;
//! // ... game materializes the scene and builds its node/material key maps ...
//! let mut keys = Vec::new();
//! for clip in &project.editor_animations {
//!     let group = lower_stored_clip(clip, |t| resolve_target(t, &node_map, &mat_map));
//!     keys.push((clip.id, renderer.animations.insert_clip(group)));
//! }
//! renderer.animations.mixer = lower_stored_mixer(
//!     &project.anim_mixer,
//!     |id| keys.iter().find(|(a, _)| *a == id).map(|(_, k)| *k),
//!     |nodes, _desc| TargetMask { transforms: resolve_mask(nodes, &node_map) },
//! );
//! // then drive it: renderer.update_animations(dt)? each frame.
//! ```

use awsm_scene_schema::animation::{
    ClipDirection, ClipLoop, LayerModeDoc, MixerDoc, SamplerKind, StoredAnimation, StoredTrack,
    TrackTarget, TrackValue, TransformProp,
};
use awsm_scene_schema::{AssetId, NodeId};
use glam::{Quat, Vec3};

use super::{
    AnimationChannel, AnimationClipGroup, AnimationClipKey, AnimationData, AnimationLayer,
    AnimationLoopStyle, AnimationMixer, AnimationPlayDirection, AnimationSampler, AnimationStrip,
    AnimationTarget, TargetMask, TransformAnimation, VertexAnimation,
};

/// Lower one authored clip into a runtime [`AnimationClipGroup`].
///
/// `resolve` maps each track's abstract [`TrackTarget`] to a concrete
/// [`AnimationTarget`]; tracks that don't resolve (target gone) or have no
/// keyframes are skipped, and `mute`d tracks are dropped. The group's
/// loop/speed/direction come straight from the stored clip. (Per-track `solo` is
/// an editor focus state and is intentionally ignored at runtime — a game plays
/// every non-muted track.)
pub fn lower_stored_clip(
    clip: &StoredAnimation,
    resolve: impl Fn(&TrackTarget) -> Option<AnimationTarget>,
) -> AnimationClipGroup {
    let channels: Vec<AnimationChannel> = clip
        .tracks
        .iter()
        .filter(|t| !t.mute)
        .filter_map(|t| lower_stored_track(t, &resolve))
        .collect();

    let mut group = AnimationClipGroup::new(clip.name.clone(), clip.duration, channels);
    group.loop_style = match clip.loop_style {
        ClipLoop::Loop => Some(AnimationLoopStyle::Loop),
        ClipLoop::PingPong => Some(AnimationLoopStyle::PingPong),
        ClipLoop::Once => None,
    };
    group.speed = clip.speed;
    group.play_direction = match clip.direction {
        ClipDirection::Forward => AnimationPlayDirection::Forward,
        ClipDirection::Reverse => AnimationPlayDirection::Backward,
    };
    group
}

/// Lower one stored track into an [`AnimationChannel`], or `None` if its target
/// is unresolved or it has no (aligned) keyframes.
fn lower_stored_track(
    track: &StoredTrack,
    resolve: &impl Fn(&TrackTarget) -> Option<AnimationTarget>,
) -> Option<AnimationChannel> {
    let target = resolve(&track.target)?;
    if track.times.is_empty() || track.keys.len() != track.times.len() {
        return None;
    }

    let prop = match &track.target {
        TrackTarget::Transform { prop, .. } => Some(*prop),
        _ => None,
    };
    // A morph track keys one scalar weight per keyframe but the renderer consumes
    // the whole weight vector; reconcile per keyframe (mirrors the editor).
    let morph_index = match &track.target {
        TrackTarget::Morph { index, .. } => Some(*index),
        _ => None,
    };
    let to_data = |v: &TrackValue| -> AnimationData {
        match morph_index {
            Some(i) => morph_scalar_to_vertex(v, i),
            None => track_value_to_data(v, prop),
        }
    };

    let values: Vec<AnimationData> = track.keys.iter().map(|k| to_data(&k.value)).collect();
    let sampler = match track.sampler {
        SamplerKind::Linear => AnimationSampler::new_linear(track.times.clone(), values),
        SamplerKind::Step => AnimationSampler::new_step(track.times.clone(), values),
        SamplerKind::Cubic => {
            let in_tangents = track.keys.iter().map(|k| to_data(&k.in_tangent)).collect();
            let out_tangents = track.keys.iter().map(|k| to_data(&k.out_tangent)).collect();
            AnimationSampler::new_cubic_spline(
                track.times.clone(),
                values,
                in_tangents,
                out_tangents,
            )
        }
    };

    Some(AnimationChannel::new(target, sampler))
}

/// Build a runtime [`AnimationMixer`] from the authored [`MixerDoc`].
///
/// `clip_key` looks up the inserted [`AnimationClipKey`] for a clip `AssetId`;
/// strips (and additive base clips) referencing an unknown clip are dropped.
/// `mask` resolves a layer's node set (+ include-descendants flag) into a
/// [`TargetMask`] — only called for layers that declare a non-empty mask, so the
/// caller can expand descendants against its own scene hierarchy.
pub fn lower_stored_mixer(
    doc: &MixerDoc,
    clip_key: impl Fn(AssetId) -> Option<AnimationClipKey>,
    mask: impl Fn(&[NodeId], bool) -> TargetMask,
) -> AnimationMixer {
    let mut mixer = AnimationMixer::new();
    for layer in &doc.layers {
        let strips: Vec<AnimationStrip> = layer
            .strips
            .iter()
            .filter_map(|s| {
                clip_key(s.clip).map(|key| AnimationStrip {
                    clip: key,
                    start: s.start,
                    len: s.len,
                    scale: s.scale,
                    repeat: s.repeat,
                })
            })
            .collect();

        let mut runtime_layer = match layer.mode {
            LayerModeDoc::Replace => AnimationLayer::new_replace(strips),
            LayerModeDoc::Additive { base_clip } => {
                AnimationLayer::new_additive(base_clip.and_then(&clip_key), strips)
            }
        };
        runtime_layer.weight = layer.weight;
        if !layer.mask_nodes.is_empty() {
            runtime_layer.mask = Some(mask(&layer.mask_nodes, layer.include_descendants));
        }
        mixer.layers.push(runtime_layer);
    }
    mixer
}

/// Lower one authored [`TrackValue`] → renderer [`AnimationData`]. Transform
/// tracks lower to a per-field [`TransformAnimation`] (only the track's own
/// component), so e.g. a rotation track leaves translation/scale untouched;
/// everything else lowers straight to scalar/vec3/quat.
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
        (None, TrackValue::Scalar(s)) => AnimationData::F32(*s),
        (None, TrackValue::Vec3(v)) => AnimationData::Vec3(Vec3::from_array(*v)),
        (None, TrackValue::Quat(q)) => AnimationData::Quat(Quat::from_array(*q)),
        // Shape mismatch — lower to inert data rather than panicking (the editor
        // validates genuine mismatches as hard errors before saving).
        (Some(_), TrackValue::Scalar(s)) => AnimationData::F32(*s),
        (Some(TransformProp::Translation) | Some(TransformProp::Scale), TrackValue::Quat(q)) => {
            AnimationData::Quat(Quat::from_array(*q))
        }
        (Some(TransformProp::Rotation), TrackValue::Vec3(v)) => {
            AnimationData::Vec3(Vec3::from_array(*v))
        }
    }
}

/// Lower one morph keyframe (a single scalar weight at `index`) into the
/// [`AnimationData::Vertex`] weight vector the morph target consumes: length
/// `index + 1`, position `index` carrying the scalar, the rest `0`.
fn morph_scalar_to_vertex(value: &TrackValue, index: usize) -> AnimationData {
    let scalar = match value {
        TrackValue::Scalar(s) => *s,
        TrackValue::Vec3(v) => v.first().copied().unwrap_or(0.0),
        TrackValue::Quat(q) => q.first().copied().unwrap_or(0.0),
    };
    let mut weights = vec![0.0_f32; index + 1];
    weights[index] = scalar;
    AnimationData::Vertex(VertexAnimation::new(weights))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transforms::TransformKey;
    use awsm_scene_schema::animation::{Interp, Keyframe};

    fn vec3_key(v: [f32; 3]) -> Keyframe {
        Keyframe {
            value: TrackValue::Vec3(v),
            interp: Interp::Linear,
            in_tangent: TrackValue::Vec3([0.0; 3]),
            out_tangent: TrackValue::Vec3([0.0; 3]),
        }
    }

    /// A stored translation clip lowers to a group that samples to the linearly
    /// interpolated pose — the core runtime path a game relies on, with no GPU.
    #[test]
    fn stored_translation_clip_lowers_and_samples() {
        let node = NodeId::new();
        let stored = StoredAnimation {
            id: AssetId::new(),
            name: "Walk".into(),
            duration: 1.0,
            loop_style: ClipLoop::Loop,
            speed: 1.0,
            direction: ClipDirection::Forward,
            color: String::new(),
            tracks: vec![StoredTrack {
                target: TrackTarget::Transform {
                    node,
                    prop: TransformProp::Translation,
                },
                sampler: SamplerKind::Linear,
                mute: false,
                solo: false,
                expanded: false,
                times: vec![0.0, 1.0],
                keys: vec![vec3_key([0.0, 0.0, 0.0]), vec3_key([0.0, 10.0, 0.0])],
            }],
        };

        // Resolver maps the (single) transform target to a placeholder key.
        let key = TransformKey::default();
        let group = lower_stored_clip(&stored, |_t| Some(AnimationTarget::Transform(key)));

        assert_eq!(group.channels.len(), 1);
        assert_eq!(group.loop_style, Some(AnimationLoopStyle::Loop));

        // Sample at t=0.5 → translation [0, 5, 0].
        let mut got = None;
        group.for_each_sample_at(0.5, |_target, data| got = Some(data));
        match got {
            Some(AnimationData::Transform(t)) => {
                let tr = t.translation.expect("translation present");
                assert!((tr.x - 0.0).abs() < 1e-5);
                assert!((tr.y - 5.0).abs() < 1e-5);
                assert!((tr.z - 0.0).abs() < 1e-5);
            }
            other => panic!("expected Transform translation, got {other:?}"),
        }
    }

    /// A muted track is dropped; an unresolved target is skipped.
    #[test]
    fn muted_and_unresolved_tracks_are_skipped() {
        let stored = StoredAnimation {
            id: AssetId::new(),
            name: "C".into(),
            duration: 1.0,
            loop_style: ClipLoop::Once,
            speed: 1.0,
            direction: ClipDirection::Forward,
            color: String::new(),
            tracks: vec![StoredTrack {
                target: TrackTarget::Transform {
                    node: NodeId::new(),
                    prop: TransformProp::Translation,
                },
                sampler: SamplerKind::Linear,
                mute: true,
                solo: false,
                expanded: false,
                times: vec![0.0, 1.0],
                keys: vec![vec3_key([0.0; 3]), vec3_key([1.0; 3])],
            }],
        };
        // Muted → no channels even though the resolver would succeed.
        let group = lower_stored_clip(&stored, |_t| {
            Some(AnimationTarget::Transform(TransformKey::default()))
        });
        assert_eq!(group.channels.len(), 0);
        assert_eq!(group.loop_style, None); // Once → no loop
    }
}
