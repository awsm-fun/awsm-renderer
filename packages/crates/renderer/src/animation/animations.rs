//! Animation storage and per-frame updates.

use std::collections::HashMap;

use slotmap::{new_key_type, DenseSlotMap, SecondaryMap, SlotMap};

use crate::{
    meshes::morphs::{GeometryMorphKey, MaterialMorphKey},
    transforms::TransformKey,
    AwsmRenderer,
};

use super::blend::{blend_additive, blend_replace};
use super::clip_group::{
    AnimationClipGroup, AnimationClipKey, AnimationTarget, BuiltinMaterialParam, CameraParam,
    LightParam,
};
use super::mixer::{AnimationLayer, AnimationMixer, LayerMode};
use super::{data::AnimationData, error::Result, player::AnimationPlayer, AwsmAnimationError};

new_key_type! {
    /// SlotMap key for animation players.
    pub struct AnimationKey;
}

/// Morph targets that can be animated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimationMorphKey {
    Geometry(GeometryMorphKey),
    Material(MaterialMorphKey),
}

impl From<GeometryMorphKey> for AnimationMorphKey {
    fn from(key: GeometryMorphKey) -> Self {
        AnimationMorphKey::Geometry(key)
    }
}

impl From<MaterialMorphKey> for AnimationMorphKey {
    fn from(key: MaterialMorphKey) -> Self {
        AnimationMorphKey::Material(key)
    }
}

/// Container for animation players and their targets.
#[derive(Debug, Clone, Default)]
pub struct Animations {
    players: DenseSlotMap<AnimationKey, AnimationPlayer>,
    // Different kinds of animations:
    transforms: SecondaryMap<AnimationKey, TransformKey>,
    morphs: SecondaryMap<AnimationKey, AnimationMorphKey>,
    // Named clip groups (the editor-authored "Clip" runtime form). Each
    // group shares one clock across its channels and may drive any
    // `AnimationTarget` kind. In the mixer path the group's own
    // clock is NOT advanced — strips derive the clip-local time from the
    // mixer timeline. In the single-clip fallback (empty mixer) each group
    // advances on its own clock as an implicit whole-rig Replace layer.
    clips: SlotMap<AnimationClipKey, AnimationClipGroup>,
    /// The NLA mixer. When non-empty, it composites the clip
    /// groups via weighted/additive layers; when empty, the single-clip
    /// fallback in `update_animations` plays each clip on its own clock.
    pub mixer: AnimationMixer,
    /// Per-target **rest** (authored-default) cache. The mixer composite
    /// seeds each target's accumulator from this, NOT from the live
    /// (already-animated) value — so additive deltas don't accumulate and
    /// drift across frames. Lazily
    /// captured the first frame a target receives a contribution (before
    /// any write this frame, so the captured value is the authored
    /// default). The editor invalidates entries when authored defaults
    /// change via [`Self::invalidate_rest`] / [`Self::clear_rest_cache`].
    rest: HashMap<AnimationTarget, AnimationData>,

    // ── per-frame scratch buffers (capacity reused across frames so the
    //    composite path allocates nothing in steady state). Each is
    //    `mem::take`-n into a local at the top of `update_clip_mixer` and put
    //    back at the end; on an (exceptional) error the take is simply dropped
    //    and re-grown next frame — no correctness impact.
    /// Per-target composite accumulator.
    scratch_acc: HashMap<AnimationTarget, AnimationData>,
    /// One strip's sampled `(target, value)` pairs.
    scratch_samples: Vec<(AnimationTarget, AnimationData)>,
    /// The write-pass target list (drawn from `rest`'s keys).
    scratch_targets: Vec<AnimationTarget>,
    /// The single-clip-fallback clip-key list.
    scratch_clip_keys: Vec<AnimationClipKey>,
    /// An additive layer's base-clip pose, sampled once per strip (so the
    /// per-target reference lookup is O(1), not a full re-sample per target).
    scratch_base: HashMap<AnimationTarget, AnimationData>,
}

impl Animations {
    /// Creates an empty animation container.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a clip group and returns its key.
    pub fn insert_clip(&mut self, group: AnimationClipGroup) -> AnimationClipKey {
        self.clips.insert(group)
    }

    /// Removes a clip group by key, returning it if it existed.
    pub fn remove_clip(&mut self, key: AnimationClipKey) -> Option<AnimationClipGroup> {
        self.clips.remove(key)
    }

    /// Returns a reference to a clip group, or `None` if the key is unknown.
    pub fn get_clip(&self, key: AnimationClipKey) -> Option<&AnimationClipGroup> {
        self.clips.get(key)
    }

    /// Returns a mutable reference to a clip group, or `None` if the key is
    /// unknown.
    pub fn get_clip_mut(&mut self, key: AnimationClipKey) -> Option<&mut AnimationClipGroup> {
        self.clips.get_mut(key)
    }

    /// Iterates every clip group.
    pub fn clips_iter(&self) -> impl Iterator<Item = (AnimationClipKey, &AnimationClipGroup)> {
        self.clips.iter()
    }

    /// Iterates every clip group mutably.
    pub fn clips_iter_mut(
        &mut self,
    ) -> impl Iterator<Item = (AnimationClipKey, &mut AnimationClipGroup)> {
        self.clips.iter_mut()
    }

    /// Removes all clip groups.
    pub fn clear_clips(&mut self) {
        self.clips.clear();
    }

    /// Whether there are any clip groups.
    pub fn has_clips(&self) -> bool {
        !self.clips.is_empty()
    }

    /// Number of cached rest-pose entries (animation diagnostics): one per
    /// distinct target any lowered channel has contributed to.
    pub fn rest_len(&self) -> usize {
        self.rest.len()
    }

    /// Drops the entire rest-pose cache. The editor calls this when authored
    /// defaults may have changed wholesale (e.g. a new scene loaded); the
    /// next mixer frame re-captures rest for every contributing target.
    pub fn clear_rest_cache(&mut self) {
        self.rest.clear();
    }

    /// Drops the rest entry for a single target so it is re-captured on the
    /// next mixer frame. The editor calls this when that target's authored
    /// default changes (so the new default — not the stale one — seeds the
    /// accumulator).
    pub fn invalidate_rest(&mut self, target: AnimationTarget) {
        self.rest.remove(&target);
    }

    /// **Explicitly seed** a target's rest (authored-default) value. The editor
    /// calls this so rest comes from the authoritative authored value (e.g. a
    /// node's authored transform) rather than the renderer's lazily-read *live*
    /// local — which animation overwrites each frame (rest must be the
    /// bind/default, never the already-animated value). Overwrites any
    /// existing entry, so re-lowering refreshes rest from the authored source.
    pub fn set_rest(&mut self, target: AnimationTarget, value: AnimationData) {
        self.rest.insert(target, value);
    }

    /// Removes an animation player and its associations.
    pub fn remove(&mut self, key: AnimationKey) {
        self.players.remove(key);
        self.transforms.remove(key);
        self.morphs.remove(key);
    }

    /// Inserts a transform animation player.
    pub fn insert_transform(
        &mut self,
        player: AnimationPlayer,
        transform_key: TransformKey,
    ) -> AnimationKey {
        let key = self.players.insert(player);
        self.transforms.insert(key, transform_key);
        key
    }

    /// Inserts a morph animation player.
    pub fn insert_morph(
        &mut self,
        player: AnimationPlayer,
        morph_key: AnimationMorphKey,
    ) -> AnimationKey {
        let key = self.players.insert(player);
        self.morphs.insert(key, morph_key);
        key
    }
}

/// Coerces an [`AnimationData`] to an `f32`. `F64` is narrowed to `f32`.
/// Any non-scalar kind is a `WrongKind` error.
fn data_to_f32(value: &AnimationData) -> Result<f32> {
    match value {
        AnimationData::F32(v) => Ok(*v),
        AnimationData::F64(v) => Ok(*v as f32),
        other => Err(AwsmAnimationError::WrongKind(format!(
            "expected scalar (F32/F64) animation data, got {other:?}"
        ))),
    }
}

/// Coerces an [`AnimationData`] to a `[f32; 3]` (from a `Vec3`). Any other
/// kind is a `WrongKind` error.
fn data_to_vec3(value: &AnimationData) -> Result<[f32; 3]> {
    match value {
        AnimationData::Vec3(v) => Ok([v.x, v.y, v.z]),
        other => Err(AwsmAnimationError::WrongKind(format!(
            "expected Vec3 animation data, got {other:?}"
        ))),
    }
}

/// Coerces an [`AnimationData`] to a `[f32; 4]` — from a `Quat` (xyzw). Any
/// other kind is a `WrongKind` error. Provided as a spec helper; the Uniform
/// apply path converts `Quat`→`Vec4` directly via [`data_to_uniform_value`],
/// so this is currently only exercised by tests.
#[allow(dead_code)]
fn data_to_vec4(value: &AnimationData) -> Result<[f32; 4]> {
    match value {
        AnimationData::Quat(q) => Ok([q.x, q.y, q.z, q.w]),
        other => Err(AwsmAnimationError::WrongKind(format!(
            "expected Quat animation data, got {other:?}"
        ))),
    }
}

/// Converts an [`AnimationData`] to a dynamic-material [`UniformValue`].
/// `F32`/`F64` → `F32`, `Vec3` → `Vec3`, `Quat` → `Vec4`. Anything else is a
/// `WrongKind` error.
fn data_to_uniform_value(
    value: &AnimationData,
) -> Result<awsm_materials::dynamic_layout::UniformValue> {
    use awsm_materials::dynamic_layout::UniformValue;
    match value {
        AnimationData::F32(v) => Ok(UniformValue::F32(*v)),
        AnimationData::F64(v) => Ok(UniformValue::F32(*v as f32)),
        AnimationData::Vec3(v) => Ok(UniformValue::Vec3([v.x, v.y, v.z])),
        AnimationData::Quat(q) => Ok(UniformValue::Vec4([q.x, q.y, q.z, q.w])),
        other => Err(AwsmAnimationError::WrongKind(format!(
            "cannot convert {other:?} animation data to a UniformValue"
        ))),
    }
}

impl AwsmRenderer {
    /// Advances animation players and applies their results.
    ///
    /// `global_time_delta_ms` is the wall-clock frame delta in **milliseconds**
    /// (e.g. the difference of two rAF / `performance.now()` timestamps) — the one
    /// millisecond quantity in the system. It is converted to seconds once here;
    /// every downstream clock (loose players, clip groups, the mixer timeline)
    /// then runs in **seconds**, the same unit as glTF keyframe times and authored
    /// durations. `speed`/`scale` are pure dimensionless multipliers.
    pub fn update_animations(&mut self, global_time_delta_ms: f64) -> Result<()> {
        let dt_seconds = global_time_delta_ms / 1000.0;
        for player in self.animations.players.values_mut() {
            player.update(dt_seconds)
        }

        for (animation_key, transform_key) in self.animations.transforms.iter() {
            let player = self
                .animations
                .players
                .get(animation_key)
                .ok_or(AwsmAnimationError::MissingKey(animation_key))?;
            // A loose player whose target transform was freed must NOT abort the
            // WHOLE pose (this loop runs before the clip mixer below, so a single
            // stale channel would otherwise skip ALL animation + spam an error). A
            // scene reload frees the old skeleton's transforms before the players
            // re-bind — skip the dangling player; the relower rebinds it. Present
            // keys are byte-for-byte unchanged (same `apply` on the same `Transform`).
            let Ok(transform) = self.transforms.get_local(*transform_key).cloned() else {
                continue;
            };
            match player.sample() {
                AnimationData::Transform(transform_animation) => {
                    let updated_transform = transform_animation.apply(transform);
                    self.transforms
                        .set_local(*transform_key, updated_transform)?;
                }
                _ => {
                    return Err(AwsmAnimationError::WrongKind("weird, animation player has a transform key but the animation data is not a transform".to_string()));
                }
            }
        }

        for (animation_key, morph_key) in self.animations.morphs.iter() {
            let player = self
                .animations
                .players
                .get(animation_key)
                .ok_or(AwsmAnimationError::MissingKey(animation_key))?;

            match player.sample() {
                // A loose player whose target MORPH was freed must NOT abort the whole
                // pose (a scene reload frees the morph before the player re-binds) —
                // skip the dangling write; the relower rebinds it. Present keys unchanged.
                AnimationData::Vertex(vertex_animation) => match morph_key {
                    AnimationMorphKey::Geometry(morph_key) => {
                        let _ = self.meshes.morphs.geometry.update_morph_weights_with(
                            *morph_key,
                            |target| {
                                target.copy_from_slice(&vertex_animation.weights);
                            },
                        );
                    }
                    AnimationMorphKey::Material(morph_key) => {
                        let _ = self.meshes.morphs.material.update_morph_weights_with(
                            *morph_key,
                            |target| {
                                target.copy_from_slice(&vertex_animation.weights);
                            },
                        );
                    }
                },
                _ => {
                    return Err(AwsmAnimationError::WrongKind("weird, animation player has a mesh key but the animation data is not for a mesh".to_string()));
                }
            }
        }

        // ---- Clip-group processing (NLA mixer) --------------------------
        // Accumulate-then-write: every animated target's accumulator is
        // seeded from its REST value (the authored default, captured once,
        // so additive deltas don't drift), each active layer's contribution
        // is composited in order, and the result is written ONCE per target.
        // The loose-player path above is left byte-for-byte unchanged so
        // single-channel models (e.g. the Fox) stay bit-identical.
        self.update_clip_mixer(dt_seconds)?;

        Ok(())
    }

    /// The NLA-mixer composite. Selects between the mixer path (layers/strips
    /// drive clip-local time off the shared mixer timeline) and the
    /// single-clip fallback (no mixer ⇒ each clip plays on its own clock as an
    /// implicit whole-rig Replace layer).
    fn update_clip_mixer(&mut self, dt_seconds: f64) -> Result<()> {
        // Nothing to do if there are no clip groups at all.
        if !self.animations.has_clips() {
            return Ok(());
        }

        let use_mixer = !self.animations.mixer.is_empty();

        // 1. Advance the relevant clock(s).
        if use_mixer {
            self.animations.mixer.advance(dt_seconds);
        } else {
            // Single-clip fallback: advance each clip group's own clock.
            for (_, group) in self.animations.clips_iter_mut() {
                group.update(dt_seconds);
            }
        }

        // 2. Composite, per target, every active layer/clip contribution into
        //    `acc` (seeded from each target's rest value). Targets in `rest` that
        //    receive NO contribution this frame still write their rest value back
        //    (write pass) so disabling a layer restores the default.
        //
        //    The accumulator + sample buffers are `mem::take`-n from `self` so
        //    they're owned locals here (lets us borrow `self` freely while using
        //    them); their capacity is reused across frames, so the steady-state
        //    composite allocates nothing. An exceptional early-return just drops
        //    them — they re-grow next frame, no correctness impact.
        let mut acc = std::mem::take(&mut self.animations.scratch_acc);
        let mut samples = std::mem::take(&mut self.animations.scratch_samples);
        let mut base = std::mem::take(&mut self.animations.scratch_base);
        acc.clear();

        if use_mixer {
            let mixer_time = self.animations.mixer.time();
            // Move the layer stack out (vs cloning its strips + mask sets EVERY
            // frame) and restore it UNCONDITIONALLY after compositing — including
            // when the helper returns early via `?`.
            let layers = std::mem::take(&mut self.animations.mixer.layers);
            let result =
                self.composite_mixer_layers(&layers, mixer_time, &mut acc, &mut samples, &mut base);
            self.animations.mixer.layers = layers;
            result?;
        } else {
            // Single-clip fallback: every clip is an implicit whole-rig Replace
            // layer at weight 1.0 on its OWN clock.
            let mut clip_keys = std::mem::take(&mut self.animations.scratch_clip_keys);
            clip_keys.clear();
            clip_keys.extend(self.animations.clips_iter().map(|(k, _)| k));
            for &key in &clip_keys {
                samples.clear();
                if let Some(group) = self.animations.get_clip(key) {
                    group.for_each_sample(|t, v| samples.push((t, v)));
                }
                for (target, value) in samples.iter() {
                    let target = *target;
                    self.ensure_rest(target)?;
                    let rest_val = match self.animations.rest.get(&target) {
                        Some(r) => r.clone(),
                        None => continue,
                    };
                    let entry = acc.entry(target).or_insert_with(|| rest_val.clone());
                    *entry = blend_replace(entry, value, 1.0);
                }
            }
            clip_keys.clear();
            self.animations.scratch_clip_keys = clip_keys;
        }

        // 3. Write once per target. A target that received a contribution writes
        //    its composited accumulator; one that received NONE this frame writes
        //    its REST value back so the default is restored rather than frozen.
        let mut targets = std::mem::take(&mut self.animations.scratch_targets);
        targets.clear();
        targets.extend(self.animations.rest.keys().copied());
        for &target in &targets {
            let value = match acc.get(&target) {
                Some(v) => v.clone(),
                None => self
                    .animations
                    .rest
                    .get(&target)
                    .cloned()
                    .expect("rest entry exists for a key drawn from rest"),
            };
            self.write_anim_target(target, &value)?;
        }

        // Return the scratch buffers (cleared — capacity retained for next frame).
        acc.clear();
        samples.clear();
        base.clear();
        targets.clear();
        self.animations.scratch_acc = acc;
        self.animations.scratch_samples = samples;
        self.animations.scratch_base = base;
        self.animations.scratch_targets = targets;

        Ok(())
    }

    /// Composite every active strip contribution from `layers` into `acc`. Split
    /// out of [`Self::update_clip_mixer`] so the caller can `mem::take` the layer
    /// stack (avoiding a per-frame clone of the strips + mask sets) and restore
    /// it unconditionally — including across the `?` below. `samples` / `base`
    /// are reused scratch buffers, cleared per strip.
    fn composite_mixer_layers(
        &mut self,
        layers: &[AnimationLayer],
        mixer_time: f64,
        acc: &mut HashMap<AnimationTarget, AnimationData>,
        samples: &mut Vec<(AnimationTarget, AnimationData)>,
        base: &mut HashMap<AnimationTarget, AnimationData>,
    ) -> Result<()> {
        for layer in layers {
            let w = layer.weight as f32;
            for strip in &layer.strips {
                if !strip.is_active(mixer_time) {
                    continue;
                }
                let Some(duration) = self.animations.get_clip(strip.clip).map(|g| g.duration)
                else {
                    continue;
                };
                let local = strip.local_time(mixer_time, duration);

                // Sample this strip's channels into the reusable buffer (releases
                // the clip borrow before we touch `self`/`acc`).
                samples.clear();
                if let Some(group) = self.animations.get_clip(strip.clip) {
                    group.for_each_sample_at(local, |t, v| samples.push((t, v)));
                }

                // Additive-with-base: sample the base pose ONCE (O(channels)) so
                // the per-target reference lookup below is O(1) — not a full clip
                // re-sample per target (was O(channels²) for such a strip).
                let base_clip = match &layer.mode {
                    LayerMode::Additive { base_clip: Some(b) } => Some(*b),
                    _ => None,
                };
                base.clear();
                if let Some(b) = base_clip {
                    if let Some(group) = self.animations.get_clip(b) {
                        group.for_each_sample_at(local, |t, v| {
                            base.insert(t, v);
                        });
                    }
                }

                for (target, value) in samples.iter() {
                    let target = *target;
                    // Layer mask gates transform targets.
                    if !layer.admits(target) {
                        continue;
                    }
                    // Lazy-capture rest BEFORE any write this frame.
                    self.ensure_rest(target)?;
                    let rest_val = match self.animations.rest.get(&target) {
                        Some(r) => r.clone(),
                        None => continue, // unreadable target — skip
                    };
                    let entry = acc.entry(target).or_insert_with(|| rest_val.clone());
                    match &layer.mode {
                        LayerMode::Replace => {
                            *entry = blend_replace(entry, value, w);
                        }
                        LayerMode::Additive { base_clip } => {
                            let reference = match base_clip {
                                Some(_) => base
                                    .get(&target)
                                    .cloned()
                                    .unwrap_or_else(|| rest_val.clone()),
                                None => rest_val.clone(),
                            };
                            *entry = blend_additive(entry, value, &reference, w);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Lazily captures the rest (authored-default) value for `target` into the
    /// cache if absent. Reads the target's CURRENT value via the real read
    /// paths — called BEFORE any mixer write this frame, so the first capture
    /// records the authored default (not an already-animated value). A target
    /// whose key/slot is missing (unreadable) is simply not inserted.
    fn ensure_rest(&mut self, target: AnimationTarget) -> Result<()> {
        if self.animations.rest.contains_key(&target) {
            return Ok(());
        }
        if let Some(value) = self.read_rest(target) {
            self.animations.rest.insert(target, value);
        }
        Ok(())
    }

    /// Reads `target`'s current value as [`AnimationData`] via the real read
    /// paths. Returns `None` if the key/slot is missing or the value is not
    /// representable as animation data.
    fn read_rest(&self, target: AnimationTarget) -> Option<AnimationData> {
        match target {
            AnimationTarget::Transform(key) => {
                let t = self.transforms.get_local(key).ok()?;
                Some(AnimationData::Transform(
                    crate::animation::TransformAnimation {
                        translation: Some(t.translation),
                        rotation: Some(t.rotation),
                        scale: Some(t.scale),
                    },
                ))
            }
            AnimationTarget::Morph(morph_key) => {
                let weights = match morph_key {
                    AnimationMorphKey::Geometry(k) => {
                        self.meshes.morphs.geometry.read_morph_weights(k).ok()?
                    }
                    AnimationMorphKey::Material(k) => {
                        self.meshes.morphs.material.read_morph_weights(k).ok()?
                    }
                };
                Some(AnimationData::Vertex(
                    crate::animation::VertexAnimation::new(weights),
                ))
            }
            AnimationTarget::Uniform { material, slot } => {
                use awsm_materials::dynamic_layout::UniformValue;
                let m = self.materials.get(material).ok()?;
                let crate::materials::Material::Custom(dynamic) = m else {
                    return None;
                };
                match dynamic.values.get(slot)? {
                    UniformValue::F32(v) => Some(AnimationData::F32(*v)),
                    UniformValue::Vec3(v) | UniformValue::Color3(v) => {
                        Some(AnimationData::Vec3(glam::Vec3::from_array(*v)))
                    }
                    UniformValue::Vec4(v) | UniformValue::Color4(v) => Some(AnimationData::Quat(
                        glam::Quat::from_xyzw(v[0], v[1], v[2], v[3]),
                    )),
                    // Other uniform kinds are not animatable via the mixer.
                    _ => None,
                }
            }
            AnimationTarget::BuiltinParam { material, param } => {
                use crate::materials::Material;
                let m = self.materials.get(material).ok()?;
                match param {
                    BuiltinMaterialParam::BaseColor => {
                        let rgb = match m {
                            Material::Pbr(p) => &p.base_color_factor[0..3],
                            Material::Unlit(u) => &u.base_color_factor[0..3],
                            Material::Toon(t) => &t.base_color_factor[0..3],
                            _ => return None,
                        };
                        Some(AnimationData::Vec3(glam::Vec3::new(rgb[0], rgb[1], rgb[2])))
                    }
                    BuiltinMaterialParam::Emissive => {
                        let rgb = match m {
                            Material::Pbr(p) => p.emissive_factor,
                            Material::Unlit(u) => u.emissive_factor,
                            Material::Toon(t) => t.emissive_factor,
                            _ => return None,
                        };
                        Some(AnimationData::Vec3(glam::Vec3::from_array(rgb)))
                    }
                    BuiltinMaterialParam::Metallic => match m {
                        Material::Pbr(p) => Some(AnimationData::F32(p.metallic_factor)),
                        _ => None,
                    },
                    BuiltinMaterialParam::Roughness => match m {
                        Material::Pbr(p) => Some(AnimationData::F32(p.roughness_factor)),
                        _ => None,
                    },
                }
            }
            AnimationTarget::Light { light, param } => {
                use crate::lights::Light;
                let l = self.lights.get(light)?;
                match param {
                    LightParam::Color => {
                        let c = match l {
                            Light::Directional { color, .. }
                            | Light::Point { color, .. }
                            | Light::Spot { color, .. } => *color,
                        };
                        Some(AnimationData::Vec3(glam::Vec3::from_array(c)))
                    }
                    LightParam::Intensity => {
                        let i = match l {
                            Light::Directional { intensity, .. }
                            | Light::Point { intensity, .. }
                            | Light::Spot { intensity, .. } => *intensity,
                        };
                        Some(AnimationData::F32(i))
                    }
                    LightParam::Range => match l {
                        Light::Point { range, .. } | Light::Spot { range, .. } => {
                            Some(AnimationData::F32(*range))
                        }
                        Light::Directional { .. } => None,
                    },
                    LightParam::InnerAngle => match l {
                        Light::Spot { inner_angle, .. } => Some(AnimationData::F32(*inner_angle)),
                        _ => None,
                    },
                    LightParam::OuterAngle => match l {
                        Light::Spot { outer_angle, .. } => Some(AnimationData::F32(*outer_angle)),
                        _ => None,
                    },
                }
            }
            AnimationTarget::Camera { camera, param } => {
                use crate::cameras::CameraProjectionParams;
                let p = self.cameras.get(camera)?;
                match param {
                    CameraParam::FovY => match p.projection {
                        CameraProjectionParams::Perspective { fov_y_rad } => {
                            Some(AnimationData::F32(fov_y_rad))
                        }
                        CameraProjectionParams::Orthographic { .. } => None,
                    },
                    CameraParam::Near => Some(AnimationData::F32(p.near)),
                    CameraParam::Far => Some(AnimationData::F32(p.far)),
                    CameraParam::Aperture => Some(AnimationData::F32(p.aperture)),
                    CameraParam::FocusDistance => Some(AnimationData::F32(p.focus_distance)),
                }
            }
        }
    }

    /// Writes a composited `value` to `target` via the real write paths
    /// (transforms / morphs / materials / lights / cameras). Strict on a kind
    /// mismatch (`WrongKind`); a missing TARGET (transform freed) is SKIPPED, not
    /// fatal — a single dangling channel (e.g. a scene reload frees the old
    /// skeleton before the relower rebinds) must not abort the whole pose.
    fn write_anim_target(&mut self, target: AnimationTarget, value: &AnimationData) -> Result<()> {
        match target {
            AnimationTarget::Transform(transform_key) => {
                // Skip a channel whose target transform no longer exists (the relower
                // rebinds it next tick); present keys are byte-for-byte unchanged.
                let Ok(transform) = self.transforms.get_local(transform_key).cloned() else {
                    return Ok(());
                };
                match value {
                    AnimationData::Transform(transform_animation) => {
                        let updated = transform_animation.apply(transform);
                        self.transforms.set_local(transform_key, updated)?;
                    }
                    _ => {
                        return Err(AwsmAnimationError::WrongKind(
                            "clip channel targets a transform but the composited data is not a transform".to_string(),
                        ));
                    }
                }
            }
            AnimationTarget::Morph(morph_key) => match value {
                // Skip a stale morph target (freed on reload before the relower rebinds)
                // rather than aborting the whole pose — same robustness as the transform
                // path; present keys are unchanged.
                AnimationData::Vertex(vertex_animation) => match morph_key {
                    AnimationMorphKey::Geometry(morph_key) => {
                        let _ = self.meshes.morphs.geometry.update_morph_weights_with(
                            morph_key,
                            |target| {
                                let n = target.len().min(vertex_animation.weights.len());
                                target[..n].copy_from_slice(&vertex_animation.weights[..n]);
                            },
                        );
                    }
                    AnimationMorphKey::Material(morph_key) => {
                        let _ = self.meshes.morphs.material.update_morph_weights_with(
                            morph_key,
                            |target| {
                                let n = target.len().min(vertex_animation.weights.len());
                                target[..n].copy_from_slice(&vertex_animation.weights[..n]);
                            },
                        );
                    }
                },
                _ => {
                    return Err(AwsmAnimationError::WrongKind(
                        "clip channel targets a morph but the composited data is not a vertex animation".to_string(),
                    ));
                }
            },
            AnimationTarget::Uniform { material, slot } => {
                let uniform_value = data_to_uniform_value(value)?;
                self.update_material(material, |m| {
                    if let crate::materials::Material::Custom(dynamic) = m {
                        if let Some(slot_value) = dynamic.values.get_mut(slot) {
                            *slot_value = uniform_value.clone();
                        }
                    }
                });
            }
            AnimationTarget::BuiltinParam { material, param } => {
                self.apply_builtin_material_param(material, param, value)?;
            }
            AnimationTarget::Light { light, param } => {
                apply_light_param(&mut self.lights, light, param, value)?;
            }
            AnimationTarget::Camera { camera, param } => {
                apply_camera_param(&mut self.cameras, camera, param, value)?;
            }
        }
        Ok(())
    }

    /// Applies a [`BuiltinMaterialParam`] sample to a material. Params a
    /// material kind lacks are no-ops; wrong `AnimationData` kind is an error.
    fn apply_builtin_material_param(
        &mut self,
        material: crate::materials::MaterialKey,
        param: BuiltinMaterialParam,
        value: &AnimationData,
    ) -> Result<()> {
        use crate::materials::Material;

        // Coerce up front so a kind mismatch fails hard (rather than silently
        // inside the no-op closure).
        match param {
            BuiltinMaterialParam::BaseColor | BuiltinMaterialParam::Emissive => {
                let rgb = data_to_vec3(value)?;
                self.update_material(material, |m| match (m, param) {
                    (Material::Pbr(pbr), BuiltinMaterialParam::BaseColor) => {
                        let a = pbr.base_color_factor[3];
                        pbr.base_color_factor = [rgb[0], rgb[1], rgb[2], a];
                    }
                    (Material::Pbr(pbr), BuiltinMaterialParam::Emissive) => {
                        pbr.emissive_factor = rgb;
                    }
                    (Material::Unlit(unlit), BuiltinMaterialParam::BaseColor) => {
                        let a = unlit.base_color_factor[3];
                        unlit.base_color_factor = [rgb[0], rgb[1], rgb[2], a];
                    }
                    (Material::Unlit(unlit), BuiltinMaterialParam::Emissive) => {
                        unlit.emissive_factor = rgb;
                    }
                    (Material::Toon(toon), BuiltinMaterialParam::BaseColor) => {
                        let a = toon.base_color_factor[3];
                        toon.base_color_factor = [rgb[0], rgb[1], rgb[2], a];
                    }
                    (Material::Toon(toon), BuiltinMaterialParam::Emissive) => {
                        toon.emissive_factor = rgb;
                    }
                    // Other (material, param) combinations: no-op.
                    _ => {}
                });
            }
            BuiltinMaterialParam::Metallic | BuiltinMaterialParam::Roughness => {
                let scalar = data_to_f32(value)?;
                self.update_material(material, |m| {
                    if let Material::Pbr(pbr) = m {
                        match param {
                            BuiltinMaterialParam::Metallic => pbr.metallic_factor = scalar,
                            BuiltinMaterialParam::Roughness => pbr.roughness_factor = scalar,
                            _ => {}
                        }
                    }
                    // Unlit / Toon have no metallic/roughness: no-op.
                });
            }
        }

        Ok(())
    }
}

/// Applies a [`LightParam`] sample to a light. Params that don't apply to the
/// light's variant are no-ops; the variant itself is never changed. Wrong
/// `AnimationData` kind is an error.
fn apply_light_param(
    lights: &mut crate::lights::Lights,
    light: crate::lights::LightKey,
    param: LightParam,
    value: &AnimationData,
) -> Result<()> {
    use crate::lights::Light;

    match param {
        LightParam::Color => {
            let rgb = data_to_vec3(value)?;
            lights.update(light, |l| match l {
                Light::Directional { color, .. }
                | Light::Point { color, .. }
                | Light::Spot { color, .. } => *color = rgb,
            });
        }
        LightParam::Intensity => {
            let scalar = data_to_f32(value)?;
            lights.update(light, |l| match l {
                Light::Directional { intensity, .. }
                | Light::Point { intensity, .. }
                | Light::Spot { intensity, .. } => *intensity = scalar,
            });
        }
        LightParam::Range => {
            let scalar = data_to_f32(value)?;
            lights.update(light, |l| match l {
                Light::Point { range, .. } | Light::Spot { range, .. } => *range = scalar,
                // Directional has no range: no-op.
                Light::Directional { .. } => {}
            });
        }
        LightParam::InnerAngle => {
            let scalar = data_to_f32(value)?;
            lights.update(light, |l| {
                if let Light::Spot { inner_angle, .. } = l {
                    *inner_angle = scalar;
                }
            });
        }
        LightParam::OuterAngle => {
            let scalar = data_to_f32(value)?;
            lights.update(light, |l| {
                if let Light::Spot { outer_angle, .. } = l {
                    *outer_angle = scalar;
                }
            });
        }
    }

    Ok(())
}

/// Applies a [`CameraParam`] sample to a camera. `FovY` only touches a
/// perspective camera (no-op on orthographic). Wrong `AnimationData` kind is
/// an error.
fn apply_camera_param(
    cameras: &mut crate::cameras::Cameras,
    camera: crate::cameras::CameraKey,
    param: CameraParam,
    value: &AnimationData,
) -> Result<()> {
    use crate::cameras::CameraProjectionParams;

    let scalar = data_to_f32(value)?;
    cameras.update(camera, |p| match param {
        CameraParam::FovY => {
            if let CameraProjectionParams::Perspective { fov_y_rad } = &mut p.projection {
                *fov_y_rad = scalar;
            }
            // Orthographic: no-op for FovY.
        }
        CameraParam::Near => p.near = scalar,
        CameraParam::Far => p.far = scalar,
        CameraParam::Aperture => p.aperture = scalar,
        CameraParam::FocusDistance => p.focus_distance = scalar,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use awsm_materials::dynamic_layout::UniformValue;
    use glam::{Quat, Vec3};

    #[test]
    fn data_to_f32_coerces_and_errors() {
        assert_eq!(data_to_f32(&AnimationData::F32(1.5)).unwrap(), 1.5);
        assert_eq!(data_to_f32(&AnimationData::F64(2.0)).unwrap(), 2.0_f32);
        assert!(data_to_f32(&AnimationData::Vec3(Vec3::ZERO)).is_err());
        assert!(data_to_f32(&AnimationData::Quat(Quat::IDENTITY)).is_err());
    }

    #[test]
    fn data_to_vec3_coerces_and_errors() {
        assert_eq!(
            data_to_vec3(&AnimationData::Vec3(Vec3::new(1.0, 2.0, 3.0))).unwrap(),
            [1.0, 2.0, 3.0]
        );
        assert!(data_to_vec3(&AnimationData::F32(1.0)).is_err());
        assert!(data_to_vec3(&AnimationData::Quat(Quat::IDENTITY)).is_err());
    }

    #[test]
    fn data_to_vec4_coerces_and_errors() {
        let q = Quat::from_xyzw(0.1, 0.2, 0.3, 0.4);
        assert_eq!(
            data_to_vec4(&AnimationData::Quat(q)).unwrap(),
            [0.1, 0.2, 0.3, 0.4]
        );
        assert!(data_to_vec4(&AnimationData::F32(1.0)).is_err());
        assert!(data_to_vec4(&AnimationData::Vec3(Vec3::ZERO)).is_err());
    }

    #[test]
    fn data_to_uniform_value_maps_each_kind() {
        assert!(matches!(
            data_to_uniform_value(&AnimationData::F32(1.0)).unwrap(),
            UniformValue::F32(v) if v == 1.0
        ));
        assert!(matches!(
            data_to_uniform_value(&AnimationData::F64(2.0)).unwrap(),
            UniformValue::F32(v) if v == 2.0_f32
        ));
        assert!(matches!(
            data_to_uniform_value(&AnimationData::Vec3(Vec3::new(1.0, 2.0, 3.0))).unwrap(),
            UniformValue::Vec3([1.0, 2.0, 3.0])
        ));
        assert!(matches!(
            data_to_uniform_value(&AnimationData::Quat(Quat::from_xyzw(1.0, 2.0, 3.0, 4.0)))
                .unwrap(),
            UniformValue::Vec4([1.0, 2.0, 3.0, 4.0])
        ));
        // Transform/Vertex are not convertible.
        assert!(data_to_uniform_value(&AnimationData::Vertex(
            crate::animation::VertexAnimation::new(vec![0.0])
        ))
        .is_err());
    }

    /// A clip group driving a transform channel samples the expected
    /// interpolated value (pure clip-group sampling — no GPU).
    #[test]
    fn clip_group_samples_into_animations() {
        use crate::animation::{
            AnimationChannel, AnimationClipGroup, AnimationSampler, AnimationTarget,
        };
        use crate::transforms::TransformKey;

        let channel = AnimationChannel::new(
            AnimationTarget::Transform(TransformKey::default()),
            AnimationSampler::new_linear(
                vec![0.0, 1.0],
                vec![AnimationData::F32(0.0), AnimationData::F32(10.0)],
            ),
        );
        let mut group = AnimationClipGroup::new("clip", 1.0, vec![channel]);
        group.set_local_time(0.5);

        let mut animations = Animations::new();
        assert!(!animations.has_clips());
        let key = animations.insert_clip(group);
        assert!(animations.has_clips());
        assert!(animations.get_clip(key).is_some());

        // Collect samples the way `update_animations` does.
        let mut samples = Vec::new();
        for (_, g) in animations.clips_iter() {
            g.for_each_sample(|t, v| samples.push((t, v)));
        }
        assert_eq!(samples.len(), 1);
        match &samples[0].1 {
            AnimationData::F32(v) => assert!((v - 5.0).abs() < 1e-6),
            other => panic!("expected F32, got {other:?}"),
        }

        assert!(animations.remove_clip(key).is_some());
        assert!(!animations.has_clips());
    }

    #[test]
    fn apply_camera_param_drives_each_field() {
        use crate::cameras::{CameraParams, CameraProjectionParams, Cameras};
        let mut cams = Cameras::new();
        let key = cams.insert(CameraParams {
            projection: CameraProjectionParams::Perspective { fov_y_rad: 1.0 },
            near: 0.1,
            far: 100.0,
            aperture: 5.6,
            focus_distance: 10.0,
        });
        apply_camera_param(&mut cams, key, CameraParam::FovY, &AnimationData::F32(0.5)).unwrap();
        apply_camera_param(&mut cams, key, CameraParam::Near, &AnimationData::F32(0.25)).unwrap();
        apply_camera_param(&mut cams, key, CameraParam::Far, &AnimationData::F32(250.0)).unwrap();
        apply_camera_param(
            &mut cams,
            key,
            CameraParam::Aperture,
            &AnimationData::F32(2.8),
        )
        .unwrap();
        apply_camera_param(
            &mut cams,
            key,
            CameraParam::FocusDistance,
            &AnimationData::F32(7.0),
        )
        .unwrap();
        let p = cams.get(key).unwrap();
        assert!(
            matches!(p.projection, CameraProjectionParams::Perspective { fov_y_rad } if (fov_y_rad - 0.5).abs() < 1e-6)
        );
        assert_eq!(p.near, 0.25);
        assert_eq!(p.far, 250.0);
        assert_eq!(p.aperture, 2.8);
        assert_eq!(p.focus_distance, 7.0);
    }

    #[test]
    fn apply_camera_param_fovy_noop_on_orthographic() {
        use crate::cameras::{CameraParams, CameraProjectionParams, Cameras};
        let mut cams = Cameras::new();
        let key = cams.insert(CameraParams {
            projection: CameraProjectionParams::Orthographic { half_height: 3.0 },
            near: 0.1,
            far: 100.0,
            aperture: 5.6,
            focus_distance: 10.0,
        });
        // FovY on an ortho camera is a documented no-op (must not panic / must
        // leave the projection untouched); Near still applies.
        apply_camera_param(&mut cams, key, CameraParam::FovY, &AnimationData::F32(0.9)).unwrap();
        apply_camera_param(&mut cams, key, CameraParam::Near, &AnimationData::F32(0.5)).unwrap();
        let p = cams.get(key).unwrap();
        assert!(
            matches!(p.projection, CameraProjectionParams::Orthographic { half_height } if (half_height - 3.0).abs() < 1e-6)
        );
        assert_eq!(p.near, 0.5);
    }

    #[test]
    fn apply_camera_param_rejects_wrong_data_kind() {
        use crate::cameras::{CameraParams, CameraProjectionParams, Cameras};
        let mut cams = Cameras::new();
        let key = cams.insert(CameraParams {
            projection: CameraProjectionParams::Perspective { fov_y_rad: 1.0 },
            near: 0.1,
            far: 100.0,
            aperture: 5.6,
            focus_distance: 10.0,
        });
        // A Vec3 sample on a scalar camera param is an error, not a silent
        // mis-write.
        assert!(apply_camera_param(
            &mut cams,
            key,
            CameraParam::Near,
            &AnimationData::Vec3(glam::Vec3::ZERO),
        )
        .is_err());
    }
}
