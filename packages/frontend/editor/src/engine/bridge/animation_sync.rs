//! Animation→GPU sync: observe the controller's authored clips + mixer and
//! **lower** (auto-compile, WYSIWYG) them into the renderer's clip-group + mixer
//! runtime, then **drive** the renderer clock from the transport. Mirrors
//! `node_sync.rs` (observe reactive state, materialize renderer resources).
//!
//! Load-bearing rule: nothing here mutates animation state — it only reads the
//! controller and writes the renderer. The transport (playhead/playing) is
//! controller state, and the editor owns the clock: the render loop drives
//! [`drive_clock`] which sets each clip group's local time + `update_animations`.
//!
//! Resolution policy: a `TrackTarget` whose dependency hasn't materialized yet
//! (node mid-insert, material awaiting registration) is **pending** — its
//! channel is skipped and re-lowers when the dependency appears (the observers
//! re-fire). A target that can *never* resolve (deleted node / material) is
//! **invalid** — logged via `tracing::error!`. Camera targets resolve through
//! the node's `camera_key` into the renderer cameras store; pending until the
//! Camera node materializes that slot.

use std::sync::atomic::{AtomicBool, Ordering};

use awsm_renderer::animation::{
    AnimationChannel, AnimationClipGroup, AnimationData, AnimationLayer, AnimationLoopStyle,
    AnimationMixer, AnimationPlayDirection, AnimationStrip, AnimationTarget, BuiltinMaterialParam,
    CameraParam, LayerMode, LightParam, TargetMask, TransformAnimation,
};
use futures_signals::signal::SignalExt;

use super::bridge;
use crate::controller::animation::{
    BuiltinParamKind, CameraParamKind, CustomAnimation, LayerModeDoc, LightParamKind, MixerDoc,
    TrackTarget,
};
use crate::controller::controller;
use crate::engine::context::with_renderer_mut;
use crate::engine::scene::{AssetId, NodeId};
use crate::prelude::*;

/// Begin mirroring the controller's animation library onto the renderer.
///
/// Re-lower is driven by ONE signal: the controller's `anim_revision`, which
/// `EditorController::apply` bumps for every command that
/// [`affects_animation`](crate::controller::EditorCommand::affects_animation) —
/// the active clip set, clip params, a track's sampler/mute/solo/keyframes, the
/// mixer, the solo subtree, project resets/loads/imports. Observing the single
/// counter (rather than per-field `Mutable` observers) means no edit can silently
/// skip a re-lower — the bug where `SetTrackSampler` / time-only `SetKeyframe` /
/// `SetClipDuration` left a stale lowered channel in the renderer.
pub fn start() {
    spawn_local(async move {
        controller()
            .anim_revision
            .signal()
            .for_each(|_| async {
                schedule_relower();
            })
            .await;
    });
}

thread_local! {
    static RELOWER_PENDING: AtomicBool = const { AtomicBool::new(false) };
}

/// Debounced (~200ms, like material auto-register) re-lower of the active clip +
/// mixer into the renderer. Coalesces a burst of edits into one rebuild.
///
/// `pub(crate)` because node materialization (node_sync) also nudges it: a
/// channel whose target node hadn't materialized at lower time is skipped as
/// pending, and nothing else re-fires when the node appears — an import whose
/// clips register before all its (async) bone mirrors land would otherwise
/// lower only the bones that won the race (seen live: Fox's left-leg channels
/// silently missing while the right leg animated).
pub(crate) fn schedule_relower() {
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

    // Collect the live clips up front (cheap Arc clones); the actual lowering —
    // which for `Uniform` targets needs to resolve a custom-material asset → a
    // live `MaterialKey` against `r.materials` — happens **inside** the renderer
    // guard below so the resolver has a renderer reference.
    let clips: Vec<(AssetId, std::sync::Arc<CustomAnimation>)> = clip_ids
        .iter()
        .filter_map(|id| {
            crate::controller::animation::find_clip(&ctrl.custom_animations, *id)
                .map(|clip| (*id, clip))
        })
        .collect();

    let playhead = ctrl.playhead.get();
    with_renderer_mut(move |r| {
        // Lower each clip now that the renderer is available (Uniform resolution).
        let groups: Vec<(AssetId, AnimationClipGroup)> = clips
            .iter()
            .map(|(id, clip)| (*id, lower_clip(r, clip, solo_root)))
            .collect();
        // Breadcrumb for the pose-doesn't-deform investigations: how many clips +
        // resolved channels actually lowered. 0 channels with non-zero authored
        // tracks ⇒ every track's target was pending/invalid at lower time.
        let total_channels: usize = groups.iter().map(|(_, g)| g.channels.len()).sum();
        tracing::debug!(
            "anim relower: {} clip(s), {} resolved channel(s), mixer_doc_layers={}",
            groups.len(),
            total_channels,
            mixer_doc.layers.len()
        );
        let mixer = lower_mixer(&mixer_doc, &clip_ids, &groups);

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
        // Seed the rest (authored-default) pose for every animated TRANSFORM
        // target from the EDITOR's authored node transform — NOT the renderer's
        // live local, which animation overwrites each frame. Doing this
        // on every re-lower also refreshes rest if the authored default changed.
        // (Non-transform targets keep their lazily-captured rest; additive on
        // those is rare.) Without this, re-lowering would re-capture rest from the
        // already-animated local → additive deltas collapse to zero.
        seed_transform_rests(r, &clips);
        // Re-pin the pose at the current playhead so the viewport reflects the
        // edit immediately (WYSIWYG), even while paused.
        pin_pose(r, playhead);
    })
    .await;
}

/// Seed the rest (authored-default) pose for every animated transform target
/// from the EDITOR's authored node transform — the authoritative default, which
/// animation never overwrites (the renderer's live local IS overwritten each
/// frame, so re-capturing from it would make additive deltas collapse).
fn seed_transform_rests(
    r: &mut awsm_renderer::AwsmRenderer,
    clips: &[(AssetId, std::sync::Arc<CustomAnimation>)],
) {
    let ctrl = controller();
    let scene = &ctrl.scene;
    for (_, clip) in clips {
        for track in clip.tracks.lock_ref().iter() {
            let TrackTarget::Transform { node, .. } = &track.target else {
                continue;
            };
            let node = *node;
            let tk = bridge()
                .nodes
                .lock()
                .unwrap()
                .get(&node)
                .map(|n| n.transform_key);
            let Some(tk) = tk else { continue };
            let Some(editor_node) = crate::engine::scene::mutate::find_by_id(scene, node) else {
                continue;
            };
            let t = super::node_sync::trs_to_transform(&editor_node.transform.get());
            r.animations.set_rest(
                AnimationTarget::Transform(tk),
                AnimationData::Transform(TransformAnimation {
                    translation: Some(t.translation),
                    rotation: Some(t.rotation),
                    scale: Some(t.scale),
                }),
            );
        }
    }
}

/// Lower one authored clip → a renderer [`AnimationClipGroup`]. Resolves each
/// track's target via the bridge; pending/invalid channels are skipped (logged).
/// Honors per-track solo + the Solo-subtree focus (`solo_root`): a track outside
/// the solo set is muted (skipped) so its target rest-holds.
fn lower_clip(
    r: &awsm_renderer::AwsmRenderer,
    clip: &CustomAnimation,
    solo_root: Option<NodeId>,
) -> AnimationClipGroup {
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
        if let Some(channel) = track.lower(&|t| resolve_target(r, t)) {
            channels.push(channel);
        }
    }

    let mut group = AnimationClipGroup::new(clip.name.get_cloned(), duration, channels);
    group.loop_style = match clip.loop_style.get() {
        crate::controller::animation::ClipLoop::Loop => Some(AnimationLoopStyle::Loop),
        crate::controller::animation::ClipLoop::PingPong => Some(AnimationLoopStyle::PingPong),
        crate::controller::animation::ClipLoop::Once => None,
    };
    // Authored playback-rate multiplier (1.0 == authored rate). The clock runs
    // in seconds end to end; `update_animations` converts the frame's
    // millisecond delta once, so no unit factor belongs here.
    group.speed = clip.speed.get();
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
/// missing node/material is logged.
fn resolve_target(
    r: &awsm_renderer::AwsmRenderer,
    target: &TrackTarget,
) -> Option<AnimationTarget> {
    let b = bridge();
    match target {
        TrackTarget::Transform { node, prop: _ } => {
            // Transform tracks drive the node's own transform key (T/R/S all map
            // to it; the per-field `TransformAnimation` already isolates which
            // component is written).
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
        TrackTarget::Morph { node, index: _ } => {
            // Map node → its first materialized mesh → that mesh's geometry morph
            // key. glTF mesh morph targets are geometry morphs (the common case);
            // the authored `index` selects *which* weight within the set, but the
            // renderer `Morph` target addresses the whole weight vector — the
            // per-index reconciliation happens in `Track::lower` (see the note +
            // limitation there). PENDING (None) if the mesh/morph isn't
            // materialized yet; invalid (warn) if the node id doesn't exist.
            let mesh = b
                .nodes
                .lock()
                .unwrap()
                .get(node)
                .and_then(|n| n.model_meshes.lock().unwrap().first().copied());
            match mesh {
                Some(mesh) => r
                    .meshes
                    .geometry_morph_key_for_mesh(mesh)
                    .map(|k| AnimationTarget::Morph(k.into())),
                None => {
                    if crate::engine::scene::mutate::find_by_id(&controller().scene, *node)
                        .is_none()
                    {
                        tracing::warn!("animation: morph target references missing node {node}");
                    }
                    None
                }
            }
        }
        TrackTarget::Uniform { material, name } => {
            // Custom (dynamic-WGSL) material asset → live MaterialKey + uniform
            // slot index by name.
            //   1. asset id → registered MaterialShaderId (PENDING until the
            //      material has been registered with the renderer).
            //   2. shader id → the registration's uniform layout → slot index of
            //      `name` (declared uniform order).
            //   3. shader id → a live MaterialKey: the per-mesh `Material::Custom`
            //      whose `shader_id` matches (PENDING until a mesh using it is
            //      materialized). If several meshes use the material, the first
            //      found key is driven (documented limitation: one key per track).
            let Some(shader_id) = super::dynamic::shader_id_for_asset(*material) else {
                // Not registered yet (or never authored): PENDING if the custom
                // material asset still exists, otherwise genuinely invalid.
                if crate::controller::custom_material::find_material(
                    &controller().custom_materials,
                    *material,
                )
                .is_none()
                {
                    tracing::warn!(
                        "animation: uniform target references missing material {material}"
                    );
                }
                return None;
            };
            let slot = r
                .dynamic_material_registration(shader_id)?
                .layout
                .uniforms
                .iter()
                .position(|u| u.name == *name);
            let Some(slot) = slot else {
                tracing::warn!(
                    "animation: uniform target references unknown slot '{name}' on material {material}"
                );
                return None;
            };
            material_key_for_shader(r, shader_id)
                .map(|material| AnimationTarget::Uniform { material, slot })
        }
        TrackTarget::Camera { node, param } => {
            // A Camera node's `camera_key` indexes the renderer cameras store
            // (materialized by node_sync); the channel drives that slot's params.
            // PENDING (None) if the node exists but isn't materialized yet;
            // invalid (warn) if the node id doesn't exist at all.
            let ck = b
                .nodes
                .lock()
                .unwrap()
                .get(node)
                .and_then(|n| *n.camera_key.lock().unwrap());
            match ck {
                Some(camera) => Some(AnimationTarget::Camera {
                    camera,
                    param: camera_param(*param),
                }),
                None => {
                    if crate::engine::scene::mutate::find_by_id(&controller().scene, *node)
                        .is_none()
                    {
                        tracing::warn!("animation: camera target references missing node {node}");
                    }
                    None
                }
            }
        }
    }
}

/// Find a live `MaterialKey` whose per-mesh `Material::Custom` was built from
/// `shader_id`. Returns the first match (a material assigned to multiple meshes
/// has one renderer key per mesh; a track drives one of them — see the
/// resolution note). `None` until a mesh using the material is materialized.
fn material_key_for_shader(
    r: &awsm_renderer::AwsmRenderer,
    shader_id: awsm_materials::MaterialShaderId,
) -> Option<awsm_renderer::materials::MaterialKey> {
    use awsm_renderer::materials::Material;
    r.materials.iter().find_map(|(key, mat)| match mat {
        Material::Custom(dm) if dm.shader_id == shader_id => Some(key),
        _ => None,
    })
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

fn camera_param(p: CameraParamKind) -> CameraParam {
    match p {
        CameraParamKind::FovY => CameraParam::FovY,
        CameraParamKind::Near => CameraParam::Near,
        CameraParamKind::Far => CameraParam::Far,
        CameraParamKind::Aperture => CameraParam::Aperture,
        CameraParamKind::FocusDistance => CameraParam::FocusDistance,
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
