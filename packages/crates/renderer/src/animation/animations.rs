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
use super::mixer::{AnimationMixer, LayerMode};
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
    // `AnimationTarget` kind. In the mixer path (M-R2) the group's own
    // clock is NOT advanced — strips derive the clip-local time from the
    // mixer timeline. In the single-clip fallback (empty mixer) each group
    // advances on its own clock as an implicit whole-rig Replace layer.
    clips: SlotMap<AnimationClipKey, AnimationClipGroup>,
    /// The NLA mixer (M-R2 §4.4). When non-empty, it composites the clip
    /// groups via weighted/additive layers; when empty, the single-clip
    /// fallback in `update_animations` plays each clip on its own clock.
    pub mixer: AnimationMixer,
    /// Per-target **rest** (authored-default) cache. The mixer composite
    /// seeds each target's accumulator from this, NOT from the live
    /// (already-animated) value — the no-drift invariant I1. Lazily
    /// captured the first frame a target receives a contribution (before
    /// any write this frame, so the captured value is the authored
    /// default). The editor invalidates entries when authored defaults
    /// change via [`Self::invalidate_rest`] / [`Self::clear_rest_cache`].
    rest: HashMap<AnimationTarget, AnimationData>,
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
    pub fn update_animations(&mut self, global_time_delta: f64) -> Result<()> {
        for player in self.animations.players.values_mut() {
            player.update(global_time_delta)
        }

        for (animation_key, transform_key) in self.animations.transforms.iter() {
            let player = self
                .animations
                .players
                .get(animation_key)
                .ok_or(AwsmAnimationError::MissingKey(animation_key))?;
            let transform = self.transforms.get_local(*transform_key)?;
            match player.sample() {
                AnimationData::Transform(transform_animation) => {
                    let updated_transform = transform_animation.apply(transform.clone());
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
                AnimationData::Vertex(vertex_animation) => match morph_key {
                    AnimationMorphKey::Geometry(morph_key) => {
                        self.meshes.morphs.geometry.update_morph_weights_with(
                            *morph_key,
                            |target| {
                                target.copy_from_slice(&vertex_animation.weights);
                            },
                        )?;
                    }
                    AnimationMorphKey::Material(morph_key) => {
                        self.meshes.morphs.material.update_morph_weights_with(
                            *morph_key,
                            |target| {
                                target.copy_from_slice(&vertex_animation.weights);
                            },
                        )?;
                    }
                },
                _ => {
                    return Err(AwsmAnimationError::WrongKind("weird, animation player has a mesh key but the animation data is not for a mesh".to_string()));
                }
            }
        }

        // ---- Clip-group processing (M-R2 NLA mixer) ---------------------
        // Accumulate-then-write: every animated target's accumulator is
        // seeded from its REST value (the authored default, captured once —
        // invariant I1), each active layer's contribution is composited in
        // order, and the result is written ONCE per target. The loose-player
        // path above is left byte-for-byte unchanged (invariant I4) so
        // single-channel models (e.g. the Fox) stay bit-identical.
        self.update_clip_mixer(global_time_delta)?;

        Ok(())
    }

    /// The M-R2 NLA-mixer composite. Replaces the M-R1 last-write-wins simple
    /// path. Selects between the mixer path (layers/strips drive clip-local
    /// time off the shared mixer timeline) and the single-clip fallback (no
    /// mixer ⇒ each clip plays on its own clock as an implicit whole-rig
    /// Replace layer).
    fn update_clip_mixer(&mut self, global_time_delta: f64) -> Result<()> {
        // Nothing to do if there are no clip groups at all.
        if !self.animations.has_clips() {
            return Ok(());
        }

        let use_mixer = !self.animations.mixer.is_empty();

        // 1. Advance the relevant clock(s).
        if use_mixer {
            self.animations.mixer.advance(global_time_delta);
        } else {
            // Single-clip fallback: advance each clip group's own clock.
            for (_, group) in self.animations.clips_iter_mut() {
                group.update(global_time_delta);
            }
        }

        // 2. Gather, per target, the accumulator after compositing every
        //    layer's active contributions in order. Each target's accumulator
        //    is seeded from rest (lazily captured below).
        //
        //    `contributions[target]` holds the running accumulator. Targets
        //    that exist in `rest` but receive NO contribution this frame are
        //    still written back (their rest value) so disabling a layer
        //    restores the default rather than freezing — handled in the write
        //    pass via the rest cache.
        let mut acc: HashMap<AnimationTarget, AnimationData> = HashMap::new();

        if use_mixer {
            // Snapshot the mixer (clone) so we can read clip groups + capture
            // rest (borrowing `self`) without holding a borrow on the mixer.
            let mixer_time = self.animations.mixer.time();
            let layers = self.animations.mixer.layers.clone();

            for layer in &layers {
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

                    // Collect this strip's samples first (releases the clip
                    // borrow before we mutate `self`/`acc`).
                    let mut samples: Vec<(AnimationTarget, AnimationData)> = Vec::new();
                    if let Some(group) = self.animations.get_clip(strip.clip) {
                        group.for_each_sample_at(local, |t, v| samples.push((t, v)));
                    }

                    for (target, value) in samples {
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
                                *entry = blend_replace(entry, &value, w);
                            }
                            LayerMode::Additive { base_clip } => {
                                let reference = match base_clip {
                                    Some(base) => self
                                        .sample_clip_target(*base, local, target)
                                        .unwrap_or_else(|| rest_val.clone()),
                                    None => rest_val.clone(),
                                };
                                *entry = blend_additive(entry, &value, &reference, w);
                            }
                        }
                    }
                }
            }
        } else {
            // Single-clip fallback: every clip is an implicit whole-rig
            // Replace layer at weight 1.0, sampled at its OWN local time.
            let clip_keys: Vec<AnimationClipKey> =
                self.animations.clips_iter().map(|(k, _)| k).collect();
            for key in clip_keys {
                let mut samples: Vec<(AnimationTarget, AnimationData)> = Vec::new();
                if let Some(group) = self.animations.get_clip(key) {
                    group.for_each_sample(|t, v| samples.push((t, v)));
                }
                for (target, value) in samples {
                    self.ensure_rest(target)?;
                    let rest_val = match self.animations.rest.get(&target) {
                        Some(r) => r.clone(),
                        None => continue,
                    };
                    let entry = acc.entry(target).or_insert_with(|| rest_val.clone());
                    *entry = blend_replace(entry, &value, 1.0);
                }
            }
        }

        // 3. Write once per target. Every target present in the rest cache is
        //    written each frame: a target that received a contribution writes
        //    its composited accumulator; a target that received NONE this
        //    frame (e.g. a muted strip) writes its REST value back so the
        //    default is restored rather than frozen.
        let targets: Vec<AnimationTarget> = self.animations.rest.keys().copied().collect();
        for target in targets {
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

        Ok(())
    }

    /// Samples clip group `clip` at clip-local `time` and returns the value of
    /// channel `target`, or `None` if the clip / channel is absent.
    fn sample_clip_target(
        &self,
        clip: AnimationClipKey,
        time: f64,
        target: AnimationTarget,
    ) -> Option<AnimationData> {
        let group = self.animations.get_clip(clip)?;
        let mut found = None;
        group.for_each_sample_at(time, |t, v| {
            if t == target && found.is_none() {
                found = Some(v);
            }
        });
        found
    }

    /// Lazily captures the rest (authored-default) value for `target` into the
    /// cache if absent. Reads the target's CURRENT value via the real read
    /// paths — called BEFORE any mixer write this frame, so the first capture
    /// records the authored default (I1). A target whose key/slot is missing
    /// (unreadable) is simply not inserted.
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
    /// (transforms / morphs / materials / lights / cameras). Strict (I2): a
    /// kind mismatch returns `WrongKind`; a missing transform key propagates
    /// the transform error.
    fn write_anim_target(&mut self, target: AnimationTarget, value: &AnimationData) -> Result<()> {
        match target {
            AnimationTarget::Transform(transform_key) => {
                let transform = self.transforms.get_local(transform_key)?;
                match value {
                    AnimationData::Transform(transform_animation) => {
                        let updated = transform_animation.apply(transform.clone());
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
                AnimationData::Vertex(vertex_animation) => match morph_key {
                    AnimationMorphKey::Geometry(morph_key) => {
                        self.meshes.morphs.geometry.update_morph_weights_with(
                            morph_key,
                            |target| {
                                let n = target.len().min(vertex_animation.weights.len());
                                target[..n].copy_from_slice(&vertex_animation.weights[..n]);
                            },
                        )?;
                    }
                    AnimationMorphKey::Material(morph_key) => {
                        self.meshes.morphs.material.update_morph_weights_with(
                            morph_key,
                            |target| {
                                let n = target.len().min(vertex_animation.weights.len());
                                target[..n].copy_from_slice(&vertex_animation.weights[..n]);
                            },
                        )?;
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
}
