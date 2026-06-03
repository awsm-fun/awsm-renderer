//! Animation storage and per-frame updates.

use slotmap::{new_key_type, DenseSlotMap, SecondaryMap};

use crate::{
    meshes::morphs::{GeometryMorphKey, MaterialMorphKey},
    transforms::TransformKey,
    AwsmRenderer,
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
}

impl Animations {
    /// Creates an empty animation container.
    pub fn new() -> Self {
        Self::default()
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

        Ok(())
    }
}
