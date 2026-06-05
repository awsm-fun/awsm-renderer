//! Animation→GPU sync: observe the controller's authored clips + mixer and
//! **lower** (auto-compile, WYSIWYG) them into the renderer's clip-group + mixer
//! runtime, then **drive** the renderer clock from the transport. Mirrors
//! `node_sync.rs` (observe reactive state, materialize renderer resources).
//!
//! Load-bearing rule (§0.2): nothing here mutates animation state — it only reads
//! the controller and writes the renderer. The transport (playhead/playing) is
//! controller state, and the editor owns the clock: the render loop drives
//! [`drive_clock`] which sets each clip group's local time + `update_animations`.
//!
//! Resolution policy (§4.7-I2): a `TrackTarget` whose dependency hasn't
//! materialized yet (node mid-insert, material awaiting registration) is
//! **pending** — its channel is skipped and re-lowers when the dependency appears
//! (the observers re-fire). A target that can *never* resolve (deleted node /
//! material) is **invalid** — logged via `tracing::error!`. Camera targets are
//! **deferred** (M-A3): `camera_key` materialization isn't wired yet, so they
//! resolve to `None` (pending) and log once.

use std::sync::atomic::{AtomicBool, Ordering};

use awsm_renderer::animation::{
    AnimationChannel, AnimationClipGroup, AnimationLayer, AnimationLoopStyle, AnimationMixer,
    AnimationPlayDirection, AnimationStrip, AnimationTarget, BuiltinMaterialParam, LayerMode,
    LightParam, TargetMask,
};
use futures_signals::signal::SignalExt;
use futures_signals::signal_vec::SignalVecExt;

use super::bridge;
use crate::controller::animation::{
    BuiltinParamKind, CustomAnimation, LayerModeDoc, LightParamKind, MixerDoc, TrackTarget,
};
use crate::controller::controller;
use crate::engine::context::with_renderer_mut;
use crate::engine::scene::{AssetId, NodeId};
use crate::prelude::*;

/// Begin mirroring the controller's animation library onto the renderer.
pub fn start() {
    // Re-lower whenever the library, the active clip, or the mixer changes.
    // Each observer just kicks a debounced re-lower of the whole active set —
    // simplest correct mirror (the lowering is cheap + GPU-independent).
    spawn_local(async move {
        controller()
            .custom_animations
            .signal_vec_cloned()
            .for_each(|_| async {
                schedule_relower();
            })
            .await;
    });
    spawn_local(async move {
        controller()
            .current_clip
            .signal_cloned()
            .for_each(|_| async {
                schedule_relower();
            })
            .await;
    });
    spawn_local(async move {
        controller()
            .anim_mixer
            .signal_cloned()
            .for_each(|_| async {
                schedule_relower();
            })
            .await;
    });
    spawn_local(async move {
        controller()
            .anim_solo_root
            .signal_cloned()
            .for_each(|_| async {
                schedule_relower();
            })
            .await;
    });
    // Re-lower on deep edits (a track's keys / sampler / mute) of the active clip.
    // Observe the active clip's track list; each track-list change re-arms.
    spawn_local(async move {
        controller()
            .current_clip
            .signal_cloned()
            .for_each(|id| async move {
                if let Some(clip) = id.and_then(|id| {
                    crate::controller::animation::find_clip(&controller().custom_animations, id)
                }) {
                    observe_clip_tracks(clip).await;
                }
            })
            .await;
    });
}

/// Observe a clip's tracks + each track's keys/times/sampler/mute so a deep edit
/// re-lowers. Returns when the active clip changes (the outer `for_each` re-arms).
async fn observe_clip_tracks(clip: std::sync::Arc<CustomAnimation>) {
    clip.tracks
        .signal_vec_cloned()
        .for_each(|_| async {
            schedule_relower();
            // Re-arm per-track observers each time the list changes.
            let active = controller().current_clip.get();
            if let Some(c) = active.and_then(|id| {
                crate::controller::animation::find_clip(&controller().custom_animations, id)
            }) {
                for track in c.tracks.lock_ref().iter() {
                    let t = track.clone();
                    spawn_local(async move {
                        // Fire a single relower on any of this track's edits.
                        let sig = t.keys.signal_cloned().map(|_| ());
                        sig.for_each(|_| async {
                            schedule_relower();
                        })
                        .await;
                    });
                }
            }
        })
        .await;
}

thread_local! {
    static RELOWER_PENDING: AtomicBool = const { AtomicBool::new(false) };
}

/// Debounced (~200ms, like material auto-register) re-lower of the active clip +
/// mixer into the renderer. Coalesces a burst of edits into one rebuild.
fn schedule_relower() {
    let already = RELOWER_PENDING.with(|p| p.swap(true, Ordering::SeqCst));
    if already {
        return;
    }
    spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(200).await;
        RELOWER_PENDING.with(|p| p.store(false, Ordering::SeqCst));
        relower().await;
    });
}

/// Rebuild the renderer's clip groups + mixer from the controller's active clip
/// and mixer doc. Clears all clips first (the editor owns one active authoring
/// set), then inserts the active clip (+ any clip referenced by a mixer strip).
async fn relower() {
    let ctrl = controller();
    let active_id = ctrl.current_clip.get();
    let mixer_doc = ctrl.anim_mixer.get_cloned();
    let solo_root = ctrl.anim_solo_root.get();

    // The set of clips to materialize: the active clip plus every clip a mixer
    // strip references.
    let mut clip_ids: Vec<AssetId> = Vec::new();
    if let Some(id) = active_id {
        clip_ids.push(id);
    }
    for layer in &mixer_doc.layers {
        for strip in &layer.strips {
            if !clip_ids.contains(&strip.clip) {
                clip_ids.push(strip.clip);
            }
        }
    }

    // Build each clip group on the main thread (resolving via the bridge), then
    // hand them to the renderer under the lock.
    let groups: Vec<(AssetId, AnimationClipGroup)> = clip_ids
        .iter()
        .filter_map(|id| {
            crate::controller::animation::find_clip(&ctrl.custom_animations, *id)
                .map(|clip| (*id, lower_clip(&clip, solo_root)))
        })
        .collect();

    let mixer = lower_mixer(&mixer_doc, &clip_ids, &groups);

    let playhead = ctrl.playhead.get();
    with_renderer_mut(move |r| {
        r.animations.clear_clips();
        // Capture clip-asset-id → renderer key as we insert (the mixer references
        // clips by index into `clip_ids`, resolved below).
        let mut keys = Vec::with_capacity(groups.len());
        for (id, group) in groups {
            let key = r.animations.insert_clip(group);
            keys.push((id, key));
        }
        // Rebuild the mixer, mapping the doc's clip ids → freshly inserted keys.
        r.animations.mixer = mixer.build(&keys);
        // Authored defaults may have shifted; recapture rest next frame.
        r.animations.clear_rest_cache();
        // Re-pin the pose at the current playhead so the viewport reflects the
        // edit immediately (WYSIWYG), even while paused.
        pin_pose(r, playhead);
    })
    .await;
}

/// Lower one authored clip → a renderer [`AnimationClipGroup`]. Resolves each
/// track's target via the bridge; pending/invalid channels are skipped (logged).
/// Honors per-track solo + the Solo-subtree focus (`solo_root`): a track outside
/// the solo set is muted (skipped) so its target rest-holds.
fn lower_clip(clip: &CustomAnimation, solo_root: Option<NodeId>) -> AnimationClipGroup {
    let duration = clip.duration.get();
    let any_solo = clip.tracks.lock_ref().iter().any(|t| t.solo.get());

    let mut channels: Vec<AnimationChannel> = Vec::new();
    for track in clip.tracks.lock_ref().iter() {
        // Per-track solo: if any track is soloed, only soloed tracks play.
        if any_solo && !track.solo.get() {
            continue;
        }
        // Solo-subtree focus: only tracks whose target node is under the subtree.
        if let Some(root) = solo_root {
            if !target_under_subtree(&track.target, root) {
                continue;
            }
        }
        if let Some(channel) = track.lower(&resolve_target) {
            channels.push(channel);
        }
    }

    let mut group = AnimationClipGroup::new(clip.name.get_cloned(), duration, channels);
    group.loop_style = match clip.loop_style.get() {
        crate::controller::animation::ClipLoop::Loop => Some(AnimationLoopStyle::Loop),
        crate::controller::animation::ClipLoop::PingPong => Some(AnimationLoopStyle::PingPong),
        crate::controller::animation::ClipLoop::Once => None,
    };
    // Authored speed multiplier over the base ms→s convention (1/1000).
    group.speed = clip.speed.get() / 1000.0;
    group.play_direction = match clip.direction.get() {
        crate::controller::animation::ClipDirection::Forward => AnimationPlayDirection::Forward,
        crate::controller::animation::ClipDirection::Reverse => AnimationPlayDirection::Backward,
    };
    group
}

/// Whether a track's target node is `root` or a descendant of it (Solo-subtree).
/// Non-node targets (Uniform by material id) always pass (no subtree gating).
fn target_under_subtree(target: &TrackTarget, root: NodeId) -> bool {
    let node = match target {
        TrackTarget::Transform { node, .. }
        | TrackTarget::Morph { node, .. }
        | TrackTarget::BuiltinParam { node, .. }
        | TrackTarget::Light { node, .. }
        | TrackTarget::Camera { node, .. } => *node,
        TrackTarget::Uniform { .. } => return true,
    };
    if node == root {
        return true;
    }
    node_is_descendant(&controller().scene, root, node)
}

/// Walk the scene tree under `root` looking for `target`.
fn node_is_descendant(scene: &crate::engine::scene::Scene, root: NodeId, target: NodeId) -> bool {
    fn walk(node: &std::sync::Arc<crate::engine::scene::node::Node>, target: NodeId) -> bool {
        for child in node.children.lock_ref().iter() {
            if child.id == target || walk(child, target) {
                return true;
            }
        }
        false
    }
    if let Some(root_node) = crate::engine::scene::mutate::find_by_id(scene, root) {
        return walk(&root_node, target);
    }
    false
}

/// Resolve an authored [`TrackTarget`] → a live renderer [`AnimationTarget`].
/// `None` = pending (dependency not materialized) or invalid; a genuinely
/// missing node/material is logged. Camera targets are deferred (M-A3).
fn resolve_target(target: &TrackTarget) -> Option<AnimationTarget> {
    let b = bridge();
    match target {
        TrackTarget::Transform { node, prop: _ } => {
            // Transform tracks drive the node's own transform key (T/R/S all map
            // to it; the per-field `TransformAnimation` already isolates which
            // component is written — invariant I3).
            let tk = b.nodes.lock().unwrap().get(node).map(|n| n.transform_key);
            match tk {
                Some(tk) => Some(AnimationTarget::Transform(tk)),
                None => {
                    // Pending if the node exists in the scene but isn't materialized
                    // yet; invalid if it doesn't exist at all.
                    if crate::engine::scene::mutate::find_by_id(&controller().scene, *node)
                        .is_none()
                    {
                        tracing::error!(
                            "animation: transform target references missing node {node}"
                        );
                    }
                    None
                }
            }
        }
        TrackTarget::BuiltinParam { node, param } => {
            // A node's first material key carries its built-in factors.
            let mk = b
                .nodes
                .lock()
                .unwrap()
                .get(node)
                .and_then(|n| n.material_keys.lock().unwrap().first().copied());
            mk.map(|material| AnimationTarget::BuiltinParam {
                material,
                param: builtin_param(*param),
            })
        }
        TrackTarget::Light { node, param } => {
            let lk = b
                .nodes
                .lock()
                .unwrap()
                .get(node)
                .and_then(|n| *n.light_key.lock().unwrap());
            lk.map(|light| AnimationTarget::Light {
                light,
                param: light_param(*param),
            })
        }
        TrackTarget::Morph { node: _, index: _ } => {
            // Morph-key resolution needs the renderer mesh→morph mapping, which the
            // bridge doesn't track per-node yet — treat as PENDING for now (M-A4).
            None
        }
        TrackTarget::Uniform {
            material: _,
            name: _,
        } => {
            // Custom-material asset → live MaterialKey + slot index resolution lands
            // with the material-target wiring (M-A4); PENDING for now.
            None
        }
        TrackTarget::Camera { node: _, param: _ } => {
            // DEFERRED to M-A3: camera_key materialization isn't wired in node_sync.
            log_camera_deferred_once();
            None
        }
    }
}

thread_local! {
    static CAMERA_DEFER_LOGGED: AtomicBool = const { AtomicBool::new(false) };
}

fn log_camera_deferred_once() {
    if !CAMERA_DEFER_LOGGED.with(|l| l.swap(true, Ordering::SeqCst)) {
        tracing::info!("animation: camera targets are deferred to M-A3 (not lowered yet)");
    }
}

fn builtin_param(p: BuiltinParamKind) -> BuiltinMaterialParam {
    match p {
        BuiltinParamKind::BaseColor => BuiltinMaterialParam::BaseColor,
        BuiltinParamKind::Metallic => BuiltinMaterialParam::Metallic,
        BuiltinParamKind::Roughness => BuiltinMaterialParam::Roughness,
        BuiltinParamKind::Emissive => BuiltinMaterialParam::Emissive,
    }
}

fn light_param(p: LightParamKind) -> LightParam {
    match p {
        LightParamKind::Intensity => LightParam::Intensity,
        LightParamKind::Color => LightParam::Color,
        LightParamKind::Range => LightParam::Range,
        LightParamKind::InnerAngle => LightParam::InnerAngle,
        LightParamKind::OuterAngle => LightParam::OuterAngle,
    }
}

/// A pre-built mixer (clip refs by asset id) ready to map onto inserted keys.
struct LoweredMixer {
    layers: Vec<LoweredLayer>,
}

struct LoweredLayer {
    mode: LayerModeDoc,
    weight: f64,
    /// The masked transform-target node set (resolved → transform keys at build).
    mask_nodes: Vec<NodeId>,
    has_mask: bool,
    strips: Vec<(AssetId, f64, f64, f64, bool)>, // (clip, start, len, scale, repeat)
}

impl LoweredMixer {
    /// Realize the mixer against the freshly inserted (asset id → key) mapping.
    fn build(
        self,
        keys: &[(AssetId, awsm_renderer::animation::AnimationClipKey)],
    ) -> AnimationMixer {
        let key_for = |id: AssetId| keys.iter().find(|(a, _)| *a == id).map(|(_, k)| *k);
        let mut mixer = AnimationMixer::new();
        for layer in self.layers {
            let strips: Vec<AnimationStrip> = layer
                .strips
                .into_iter()
                .filter_map(|(clip, start, len, scale, repeat)| {
                    key_for(clip).map(|clip| AnimationStrip {
                        clip,
                        start,
                        len,
                        scale,
                        repeat,
                    })
                })
                .collect();
            let mode = match layer.mode {
                LayerModeDoc::Replace => LayerMode::Replace,
                LayerModeDoc::Additive { base_clip } => LayerMode::Additive {
                    base_clip: base_clip.and_then(key_for),
                },
            };
            let mask = if layer.has_mask {
                let mut m = TargetMask::default();
                let b = bridge();
                let nodes = b.nodes.lock().unwrap();
                for nid in &layer.mask_nodes {
                    if let Some(n) = nodes.get(nid) {
                        m.transforms.insert(n.transform_key);
                    }
                }
                Some(m)
            } else {
                None
            };
            mixer.layers.push(AnimationLayer {
                mode,
                weight: layer.weight,
                mask,
                strips,
            });
        }
        mixer
    }
}

/// Pre-lower the mixer doc into clip-id-keyed form (resolved to keys in `build`).
/// The `_groups` are unused here (kept for symmetry / future per-layer checks).
fn lower_mixer(
    doc: &MixerDoc,
    _clip_ids: &[AssetId],
    _groups: &[(AssetId, AnimationClipGroup)],
) -> LoweredMixer {
    let layers = doc
        .layers
        .iter()
        .map(|l| {
            // A layer with a non-empty node set + the descendants toggle expands to
            // every node under the masked roots.
            let mask_nodes = if l.include_descendants {
                expand_descendants(&l.mask_nodes)
            } else {
                l.mask_nodes.clone()
            };
            LoweredLayer {
                mode: l.mode,
                weight: l.weight,
                has_mask: !l.mask_nodes.is_empty(),
                mask_nodes,
                strips: l
                    .strips
                    .iter()
                    .map(|s| (s.clip, s.start, s.len, s.scale, s.repeat))
                    .collect(),
            }
        })
        .collect();
    LoweredMixer { layers }
}

/// Expand a set of root nodes to include all their descendants (for an
/// include-descendants bone mask).
fn expand_descendants(roots: &[NodeId]) -> Vec<NodeId> {
    fn collect(node: &std::sync::Arc<crate::engine::scene::node::Node>, out: &mut Vec<NodeId>) {
        for child in node.children.lock_ref().iter() {
            out.push(child.id);
            collect(child, out);
        }
    }
    let scene = controller().scene.clone();
    let mut out: Vec<NodeId> = Vec::new();
    for root in roots {
        out.push(*root);
        if let Some(n) = crate::engine::scene::mutate::find_by_id(&scene, *root) {
            collect(&n, &mut out);
        }
    }
    out
}

/// Pin the renderer animation pose at `playhead` (seconds) — the editor owns the
/// clock (the **Animation-pin** model): set each clip group's local time + the
/// mixer timeline to the playhead, then `update_animations(0.0)` for a one-shot
/// pose. Used for BOTH play (the render loop advances the playhead, then pins)
/// and scrub (playhead set by `SetPlayhead`, then pins). Called under the held
/// renderer guard from the render loop (before `update_transforms`).
pub fn pin_pose(r: &mut awsm_renderer::AwsmRenderer, playhead: f64) {
    if !r.animations.has_clips() {
        return;
    }
    // Clip groups + mixer carry their local time in **seconds** (`set_local_time`
    // / `set_time` are seconds; the ms→s `speed` only matters when *advancing*).
    // Pinning a one-shot pose just seeks both to the playhead and applies with a
    // zero delta (no advance), so the displayed pose is exactly the sampled clip.
    for (_, group) in r.animations.clips_iter_mut() {
        group.set_local_time(playhead);
    }
    r.animations.mixer.set_time(playhead);
    if let Err(e) = r.update_animations(0.0) {
        tracing::error!("update_animations (pin): {e}");
    }
}
