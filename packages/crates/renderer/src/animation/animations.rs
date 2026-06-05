//! Animation storage and per-frame updates.

use slotmap::{new_key_type, DenseSlotMap, SecondaryMap, SlotMap};

use crate::{
    meshes::morphs::{GeometryMorphKey, MaterialMorphKey},
    transforms::TransformKey,
    AwsmRenderer,
};

use super::clip_group::{
    AnimationClipGroup, AnimationClipKey, AnimationTarget, BuiltinMaterialParam, CameraParam,
    LightParam,
};
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
    // `AnimationTarget` kind. Processed after the loose players in
    // `update_animations` (last-write-wins for now — M-R1 simple path).
    clips: SlotMap<AnimationClipKey, AnimationClipGroup>,
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

        // ---- Clip-group processing (M-R1 simple path) -------------------
        // Advance every group's shared clock, then sample + apply. This is
        // last-write-wins: weighted/additive blending across groups lands in
        // a later milestone (M-R2). The loose-player path above is left
        // byte-for-byte unchanged (invariant I4) so single-channel models
        // (e.g. the Fox) stay bit-identical.

        // 1. Advance each group's clock.
        for (_, group) in self.animations.clips_iter_mut() {
            group.update(global_time_delta);
        }

        // 2. Collect samples (releases the `self.animations` borrow before we
        //    write to other `self` fields below).
        let mut samples: Vec<(AnimationTarget, AnimationData)> = Vec::new();
        for (_, group) in self.animations.clips_iter() {
            group.for_each_sample(|target, value| samples.push((target, value)));
        }

        // 3. Apply each sample to its target.
        for (target, value) in samples {
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
                                "clip channel targets a transform but the sampled data is not a transform".to_string(),
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
                                    target.copy_from_slice(&vertex_animation.weights);
                                },
                            )?;
                        }
                        AnimationMorphKey::Material(morph_key) => {
                            self.meshes.morphs.material.update_morph_weights_with(
                                morph_key,
                                |target| {
                                    target.copy_from_slice(&vertex_animation.weights);
                                },
                            )?;
                        }
                    },
                    _ => {
                        return Err(AwsmAnimationError::WrongKind(
                            "clip channel targets a morph but the sampled data is not a vertex animation".to_string(),
                        ));
                    }
                },
                AnimationTarget::Uniform { material, slot } => {
                    let uniform_value = data_to_uniform_value(&value)?;
                    self.update_material(material, |m| {
                        if let crate::materials::Material::Custom(dynamic) = m {
                            if let Some(slot_value) = dynamic.values.get_mut(slot) {
                                *slot_value = uniform_value.clone();
                            }
                        }
                    });
                }
                AnimationTarget::BuiltinParam { material, param } => {
                    self.apply_builtin_material_param(material, param, &value)?;
                }
                AnimationTarget::Light { light, param } => {
                    apply_light_param(&mut self.lights, light, param, &value)?;
                }
                AnimationTarget::Camera { camera, param } => {
                    apply_camera_param(&mut self.cameras, camera, param, &value)?;
                }
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
